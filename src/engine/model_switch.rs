use anyhow::{Result, anyhow, bail, ensure};
use zvec_rust::{Doc, Fts, SearchQuery};

use super::{MemoryEngine, StoredMemory, build_search_text, now_ms, stored_memory_from_doc};
use crate::config::hash_hex;
use crate::embedding::{Embedder, LlamaCppEmbedder};
use crate::embedding_generation::{
    ActiveEmbedding, EmbeddingGenerationManifest, GenerationState, ModelSwitchJournal, SwitchPhase,
    generation_manifest_path,
};
use crate::model::{
    self, ModelProfileReason, ModelSwitchCancelResponse, ModelSwitchRequest, ModelSwitchResponse,
    ModelSwitchStatusResponse,
};
use crate::storage::atomic::{remove_dir_all_durable, remove_file_durable};
use crate::storage::state::PendingDocument;
use crate::storage::zvec::{self, ensure_write_succeeded};

const REINDEX_BATCH_SIZE: usize = 8;
const MAX_SWITCH_ID_BYTES: usize = 128;

impl MemoryEngine {
    pub(crate) fn start_model_switch(
        &mut self,
        request: &ModelSwitchRequest,
    ) -> Result<ModelSwitchResponse> {
        if !request.dry_run {
            let switch_id = request
                .switch_id
                .as_deref()
                .ok_or_else(|| anyhow!("model switch ID is required for apply"))?;
            validate_switch_id(switch_id)?;
            let request_digest = hash_hex(serde_json::to_string(request)?.as_bytes());
            if let Some(existing) = self.switch_store.find(switch_id) {
                ensure!(
                    existing.request_digest == request_digest,
                    "model switch ID was already used for different material"
                );
                let preflight = model::preflight(&self.config, request)?;
                return self.switch_response(existing.clone(), preflight.preflight);
            }
            if self
                .switch_store
                .current
                .as_ref()
                .is_some_and(|active| !active.phase.is_terminal())
            {
                bail!("SWITCH_IN_PROGRESS: another model switch is already active");
            }
        }
        let preflight = model::preflight(&self.config, request)?;
        if request.dry_run {
            return Ok(preflight);
        }
        if !preflight.preflight.can_start {
            let blocker = preflight
                .preflight
                .blockers
                .first()
                .ok_or_else(|| anyhow!("model switch preflight is blocked"))?;
            bail!("{}: {}", blocker.code, blocker.message);
        }
        if let Some(expected) = &request.expected_active_generation_id {
            ensure!(
                expected == &self.active_embedding.generation_id,
                "ACTIVE_PROFILE_MISMATCH: expected active generation {expected}, found {}",
                self.active_embedding.generation_id
            );
        }
        let switch_id = request
            .switch_id
            .as_deref()
            .ok_or_else(|| anyhow!("model switch ID is required for apply"))?;
        let request_digest = hash_hex(serde_json::to_string(request)?.as_bytes());
        ensure!(
            self.state.pending_upserts.is_empty() && self.state.pending_deletes.is_empty(),
            "SOURCE_CHANGED: pending memory recovery must finish before model switching"
        );
        let source_content_digest = self.state.canonical_revision_digest()?;
        let source_stats = self.collection.stats()?;
        ensure!(
            source_stats.doc_count == u64::try_from(self.state.records.len())?,
            "SOURCE_CHANGED: active collection and lifecycle state record counts differ"
        );

        let target_generation_id = request.target_generation_id.clone().unwrap_or_else(|| {
            format!(
                "gen_{}",
                &hash_hex(
                    format!(
                        "{}\0{}\0{}\0{}",
                        switch_id,
                        request.target_profile_id,
                        self.active_embedding.generation_id,
                        self.state.generation
                    )
                    .as_bytes()
                )[..24]
            )
        });
        validate_generation_id(&target_generation_id)?;
        let direct_rollback = request.target_generation_id.is_some();
        if direct_rollback {
            if target_generation_id == "legacy" {
                let snapshot = self
                    .legacy_snapshot(&request.target_profile_id)
                    .ok_or_else(|| anyhow!("retained legacy generation was not found"))?;
                ensure!(
                    snapshot.source_content_digest == source_content_digest
                        && snapshot.source_embedding.is_some(),
                    "SOURCE_CHANGED: retained legacy generation is stale"
                );
            } else {
                let manifest = EmbeddingGenerationManifest::load(
                    &generation_manifest_path(&self.config, &target_generation_id),
                    self.config.project_id(),
                )?;
                ensure!(
                    manifest.profile_id == request.target_profile_id
                        && if manifest.source_content_digest.is_empty() {
                            manifest.source_state_revision == self.state.generation
                        } else {
                            manifest.source_content_digest == source_content_digest
                        }
                        && matches!(
                            manifest.state,
                            GenerationState::Retained | GenerationState::Complete
                        ),
                    "SOURCE_CHANGED: retained generation is stale or does not match the requested profile"
                );
            }
        }
        let now = now_ms()?;
        let job = ModelSwitchJournal {
            switch_id: switch_id.to_string(),
            request_digest,
            source_generation_id: self.active_embedding.generation_id.clone(),
            source_profile_id: self.active_embedding.profile_id.clone(),
            source_embedding: Some(self.config.embedding().clone()),
            target_generation_id,
            target_profile_id: request.target_profile_id.clone(),
            phase: if direct_rollback {
                SwitchPhase::Verifying
            } else {
                SwitchPhase::Queued
            },
            cursor: if direct_rollback {
                self.state.records.len()
            } else {
                0
            },
            completed_records: if direct_rollback {
                u64::try_from(self.state.records.len())?
            } else {
                0
            },
            total_records: u64::try_from(self.state.records.len())?,
            cancel_requested: false,
            source_state_revision: self.state.generation,
            source_content_digest,
            allow_dense_downtime: request.allow_dense_downtime,
            retain_previous: request.retain_previous,
            target_preexisting: direct_rollback,
            created_at_ms: now,
            updated_at_ms: now,
            completed_at_ms: None,
            error_code: None,
            error_message: None,
        };
        self.switch_store.replace_current(job.clone());
        self.save_switch_store()?;
        self.switch_response(job, preflight.preflight)
    }

    pub(crate) fn model_switch_status(&self, switch_id: &str) -> Result<ModelSwitchStatusResponse> {
        validate_switch_id(switch_id)?;
        let job = self
            .switch_store
            .find(switch_id)
            .ok_or_else(|| anyhow!("model switch not found: {switch_id}"))?;
        Ok(self.switch_status(job))
    }

    pub(crate) fn cancel_model_switch(
        &mut self,
        switch_id: &str,
    ) -> Result<ModelSwitchCancelResponse> {
        validate_switch_id(switch_id)?;
        let Some(job) = self.switch_store.current.as_mut() else {
            return Ok(ModelSwitchCancelResponse {
                switch_id: switch_id.to_string(),
                outcome: "not_found".to_string(),
            });
        };
        if job.switch_id != switch_id {
            return Ok(ModelSwitchCancelResponse {
                switch_id: switch_id.to_string(),
                outcome: "not_found".to_string(),
            });
        }
        let outcome = match job.phase {
            SwitchPhase::Committing => "already_committing",
            SwitchPhase::Succeeded => "already_committed",
            SwitchPhase::Cancelled | SwitchPhase::Failed => "already_terminal",
            phase if phase.can_cancel() => {
                job.cancel_requested = true;
                job.phase = SwitchPhase::CancelRequested;
                job.updated_at_ms = now_ms()?;
                "cancel_requested"
            }
            _ => "already_terminal",
        };
        self.save_switch_store()?;
        Ok(ModelSwitchCancelResponse {
            switch_id: switch_id.to_string(),
            outcome: outcome.to_string(),
        })
    }

    pub(crate) fn has_active_model_switch(&self) -> bool {
        self.switch_store
            .current
            .as_ref()
            .is_some_and(|job| !job.phase.is_terminal())
    }

    pub(crate) fn model_switch_freezes_mutations(&self) -> bool {
        self.has_active_model_switch()
    }

    pub(crate) fn active_model_switch_status(&self) -> Option<ModelSwitchStatusResponse> {
        self.switch_store
            .current
            .as_ref()
            .map(|job| self.switch_status(job))
    }

    pub(crate) fn run_model_switch_step(&mut self) -> Result<bool> {
        let Some(job) = self.switch_store.current.clone() else {
            return Ok(false);
        };
        if job.phase.is_terminal() {
            return Ok(false);
        }
        let result = self.run_model_switch_step_inner(job.clone());
        if let Err(error) = result {
            self.fail_model_switch(&job.switch_id, &error)?;
            return Err(error);
        }
        Ok(self.has_active_model_switch())
    }

    fn run_model_switch_step_inner(&mut self, job: ModelSwitchJournal) -> Result<()> {
        if job.cancel_requested || job.phase == SwitchPhase::CancelRequested {
            return self.finish_switch_cancelled();
        }
        match job.phase {
            SwitchPhase::Queued => self.transition_switch(SwitchPhase::Validating),
            SwitchPhase::Validating => {
                let request = ModelSwitchRequest {
                    target_profile_id: job.target_profile_id.clone(),
                    switch_id: Some(job.switch_id.clone()),
                    expected_active_profile_id: Some(job.source_profile_id.clone()),
                    expected_active_generation_id: Some(job.source_generation_id.clone()),
                    allow_dense_downtime: job.allow_dense_downtime,
                    dry_run: false,
                    force_rebuild: job.target_profile_id == job.source_profile_id,
                    retain_previous: job.retain_previous,
                    target_generation_id: Some(job.target_generation_id.clone()),
                };
                let preflight = model::preflight(&self.config, &request)?;
                ensure!(
                    preflight.preflight.can_start,
                    "model switch preflight became blocked"
                );
                ensure!(
                    self.active_embedding.generation_id == job.source_generation_id
                        && self.active_embedding.profile_id == job.source_profile_id,
                    "SOURCE_CHANGED: active generation changed before switch preparation"
                );
                self.transition_switch(SwitchPhase::Downloading)
            }
            SwitchPhase::Downloading => {
                let target_config = model::embedding_config_for_profile(
                    &job.target_profile_id,
                    self.config.embedding(),
                )?;
                if job.allow_dense_downtime {
                    self.embedder = None;
                }
                let target_embedder = LlamaCppEmbedder::load_verified(
                    &target_config,
                    self.config.model_cache(),
                    model::artifact_sha256(&job.target_profile_id),
                )?;
                self.switch_target_config = Some(target_config);
                self.switch_target_embedder = Some(target_embedder);
                self.transition_switch(SwitchPhase::Preparing)
            }
            SwitchPhase::Preparing => {
                self.ensure_source_unchanged(&job)?;
                self.ensure_target_runtime(&job)?;
                let target_config = self
                    .switch_target_config
                    .as_ref()
                    .ok_or_else(|| anyhow!("target embedding configuration is unavailable"))?;
                let target_embedder = self
                    .switch_target_embedder
                    .as_ref()
                    .ok_or_else(|| anyhow!("target embedding worker is unavailable"))?;
                let fingerprint = self
                    .config
                    .clone()
                    .with_embedding(target_config.clone())
                    .embedding_profile_fingerprint()?;
                let collection = zvec::open_generation_collection(
                    &self.config,
                    &job.target_generation_id,
                    &job.target_profile_id,
                    &fingerprint,
                    target_embedder.model_id(),
                    target_embedder.dimension(),
                    now_ms()?,
                )?;
                let manifest_path =
                    generation_manifest_path(&self.config, &job.target_generation_id);
                if !manifest_path.exists() {
                    EmbeddingGenerationManifest::building(
                        job.target_generation_id.clone(),
                        self.config.project_id().to_string(),
                        job.target_profile_id.clone(),
                        fingerprint,
                        model::artifact_sha256(&job.target_profile_id)
                            .unwrap_or_default()
                            .to_string(),
                        target_embedder.dimension(),
                        job.source_generation_id.clone(),
                        job.source_state_revision,
                        job.source_content_digest.clone(),
                        now_ms()?,
                    )
                    .save(&manifest_path)?;
                }
                self.switch_target_collection = Some(collection);
                self.transition_switch(SwitchPhase::Reindexing)
            }
            SwitchPhase::Reindexing => self.reindex_next_batch(&job),
            SwitchPhase::Verifying => self.verify_target_generation(&job),
            SwitchPhase::Committing => self.commit_target_generation(&job),
            SwitchPhase::Succeeded
            | SwitchPhase::Cancelled
            | SwitchPhase::Failed
            | SwitchPhase::CancelRequested => Ok(()),
        }
    }

    fn reindex_next_batch(&mut self, job: &ModelSwitchJournal) -> Result<()> {
        self.ensure_source_unchanged(job)?;
        self.ensure_target_runtime(job)?;
        let mut ids = self.state.records.keys().cloned().collect::<Vec<_>>();
        ids.sort();
        if job.cursor >= ids.len() {
            return self.transition_switch(SwitchPhase::Verifying);
        }
        let end = (job.cursor + REINDEX_BATCH_SIZE).min(ids.len());
        let batch_ids = &ids[job.cursor..end];
        let documents = self.fetch_documents(batch_ids)?;
        ensure!(
            documents.len() == batch_ids.len(),
            "SOURCE_CHANGED: active collection is missing source records"
        );
        let stored = documents
            .iter()
            .map(stored_memory_from_doc)
            .collect::<Result<Vec<_>>>()?;
        let mut pending = Vec::with_capacity(stored.len());
        for memory in stored {
            pending.push(self.embed_target_document(memory)?);
        }
        let target_dimension = self
            .switch_target_embedder
            .as_ref()
            .ok_or_else(|| anyhow!("target embedding worker is unavailable"))?
            .dimension();
        let zvec_documents = pending
            .iter()
            .map(|document| build_target_document(document, target_dimension))
            .collect::<Result<Vec<_>>>()?;
        let refs = zvec_documents.iter().collect::<Vec<_>>();
        let collection = self
            .switch_target_collection
            .as_mut()
            .ok_or_else(|| anyhow!("target generation collection is unavailable"))?;
        let write = collection.upsert(&refs)?;
        ensure_write_succeeded("write target embedding generation batch", &write)?;
        collection.flush()?;

        let mut manifest = EmbeddingGenerationManifest::load(
            &generation_manifest_path(&self.config, &job.target_generation_id),
            self.config.project_id(),
        )?;
        manifest.record_count = u64::try_from(end)?;
        manifest.save(&generation_manifest_path(
            &self.config,
            &job.target_generation_id,
        ))?;
        let current = self.current_switch_mut(&job.switch_id)?;
        current.cursor = end;
        current.completed_records = u64::try_from(end)?;
        current.updated_at_ms = now_ms()?;
        self.save_switch_store()
    }

    fn verify_target_generation(&mut self, job: &ModelSwitchJournal) -> Result<()> {
        self.ensure_source_unchanged(job)?;
        self.ensure_target_runtime(job)?;
        let mut ids = self
            .state
            .records
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        let probe_text = if let Some(probe_id) = ids.first() {
            let probe_id = (*probe_id).to_string();
            self.fetch_documents(std::slice::from_ref(&probe_id))?
                .first()
                .map(stored_memory_from_doc)
                .transpose()?
                .map(|stored| {
                    stored
                        .title
                        .split_whitespace()
                        .next()
                        .or_else(|| stored.content.split_whitespace().next())
                        .unwrap_or("memory")
                        .to_string()
                })
        } else {
            None
        };
        let probe_embedding = if let Some(probe_text) = probe_text.as_deref() {
            let _guard = self
                .inference_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let embedding = self
                .switch_target_embedder
                .as_mut()
                .ok_or_else(|| anyhow!("target embedding worker is unavailable"))?
                .embed_query(probe_text)?;
            validate_target_vector(&embedding)?;
            Some(embedding)
        } else {
            None
        };
        let collection = self
            .switch_target_collection
            .as_mut()
            .ok_or_else(|| anyhow!("target generation collection is unavailable"))?;
        collection.optimize()?;
        collection.flush()?;
        let stats = collection.stats()?;
        ensure!(
            stats.doc_count == job.total_records,
            "TARGET_VECTOR_INVALID: target record count does not match source snapshot"
        );
        let fetched = collection.fetch_with_options(&ids, Some(&["updated_at"]), false)?;
        ensure!(
            fetched.len() == ids.len(),
            "TARGET_VECTOR_INVALID: target generation record IDs do not match source snapshot"
        );
        if let Some(probe_text) = probe_text {
            let mut fts = Fts::new()?;
            fts.set_match_string(&probe_text)?;
            let mut query = SearchQuery::fts("search_text", &fts, 1)?;
            query.set_output_fields(&["content_hash"])?;
            ensure!(
                !collection.query(&query)?.is_empty(),
                "TARGET_VECTOR_INVALID: target retrieval probe returned no result"
            );
        }
        if let Some(probe_embedding) = probe_embedding {
            let mut query = SearchQuery::new("embedding", &probe_embedding, 1)?;
            query.set_output_fields(&["content_hash"])?;
            ensure!(
                !collection.query(&query)?.is_empty(),
                "TARGET_VECTOR_INVALID: target dense retrieval probe returned no result"
            );
        }
        if job.target_generation_id != "legacy" {
            let mut manifest = EmbeddingGenerationManifest::load(
                &generation_manifest_path(&self.config, &job.target_generation_id),
                self.config.project_id(),
            )?;
            manifest.record_count = stats.doc_count;
            manifest.state = GenerationState::Complete;
            manifest.completed_at_ms = Some(now_ms()?);
            manifest.save(&generation_manifest_path(
                &self.config,
                &job.target_generation_id,
            ))?;
        }
        self.transition_switch(SwitchPhase::Committing)
    }

    fn commit_target_generation(&mut self, job: &ModelSwitchJournal) -> Result<()> {
        if self.active_embedding.generation_id == job.target_generation_id {
            return self.finalize_committed_generation(job, now_ms()?);
        }
        self.ensure_source_unchanged(job)?;
        self.ensure_target_runtime(job)?;
        let target_config = self
            .switch_target_config
            .take()
            .ok_or_else(|| anyhow!("target embedding configuration is unavailable"))?;
        let target_embedder = self
            .switch_target_embedder
            .take()
            .ok_or_else(|| anyhow!("target embedding worker is unavailable"))?;
        let target_collection = self
            .switch_target_collection
            .take()
            .ok_or_else(|| anyhow!("target generation collection is unavailable"))?;
        let profile_fingerprint = self
            .config
            .clone()
            .with_embedding(target_config.clone())
            .embedding_profile_fingerprint()?;
        let now = now_ms()?;
        let next = ActiveEmbedding {
            format_version: 1,
            project_id: self.config.project_id().to_string(),
            generation_id: job.target_generation_id.clone(),
            profile_id: job.target_profile_id.clone(),
            profile_fingerprint,
            embedding_dimension: target_embedder.dimension(),
            activated_at_ms: now,
            predecessor_generation_id: Some(job.source_generation_id.clone()),
            source_state_revision: job.source_state_revision,
        };
        if job.target_generation_id != "legacy" {
            EmbeddingGenerationManifest::load(
                &generation_manifest_path(&self.config, &job.target_generation_id),
                self.config.project_id(),
            )?;
        }

        // The pointer is the recovery authority. Publish it while both manifests
        // remain openable, then make the in-memory cutover before durable
        // manifest finalization.
        let pointer_error = next
            .install(
                &self.config.active_embedding_path(),
                self.config.project_id(),
            )
            .err();
        if pointer_error.is_some()
            && !ActiveEmbedding::load(
                &self.config.active_embedding_path(),
                self.config.project_id(),
            )?
            .is_some_and(|active| active.generation_id == job.target_generation_id)
        {
            return Err(pointer_error
                .ok_or_else(|| anyhow!("active embedding pointer was not installed"))?);
        }

        self.collection = target_collection;
        self.embedder = Some(target_embedder);
        self.config.set_embedding(target_config);
        self.active_embedding = next;
        if let Some(error) = pointer_error {
            return Err(error);
        }
        self.finalize_committed_generation(job, now)
    }

    fn finalize_committed_generation(&mut self, job: &ModelSwitchJournal, now: i64) -> Result<()> {
        if job.target_generation_id != "legacy" {
            let target_path = generation_manifest_path(&self.config, &job.target_generation_id);
            let mut target =
                EmbeddingGenerationManifest::load(&target_path, self.config.project_id())?;
            target.state = GenerationState::Active;
            target.save(&target_path)?;
        }
        if job.source_generation_id != "legacy" {
            let source_path = generation_manifest_path(&self.config, &job.source_generation_id);
            ensure!(
                source_path.exists() || !job.retain_previous,
                "source generation manifest is unavailable for rollback retention"
            );
            if source_path.exists() {
                let mut source =
                    EmbeddingGenerationManifest::load(&source_path, self.config.project_id())?;
                source.source_state_revision = self.state.generation;
                source.source_content_digest = job.source_content_digest.clone();
                source.record_count = u64::try_from(self.state.records.len()).unwrap_or(u64::MAX);
                source.state = GenerationState::Retained;
                source.save(&source_path)?;
            }
        }
        if !job.retain_previous {
            if job.source_generation_id == "legacy" {
                remove_dir_all_durable(&self.config.collection_dir())?;
                remove_file_durable(&self.config.project_data_dir().join("manifest.json"))?;
            } else {
                let predecessor = self
                    .config
                    .embedding_generation_dir(&job.source_generation_id);
                remove_dir_all_durable(&predecessor)?;
            }
        }
        let current = self.current_switch_mut(&job.switch_id)?;
        current.phase = SwitchPhase::Succeeded;
        current.completed_at_ms = Some(now);
        current.updated_at_ms = now;
        current.error_code = None;
        current.error_message = None;
        self.save_switch_store()
    }

    fn embed_target_document(&mut self, memory: StoredMemory) -> Result<PendingDocument> {
        let search_text =
            build_search_text(&memory.title, memory.kind, &memory.tags, &memory.content);
        let embedding = {
            let _guard = self
                .inference_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.switch_target_embedder
                .as_mut()
                .ok_or_else(|| anyhow!("target embedding worker is unavailable"))?
                .embed_passage(&search_text)?
        };
        validate_target_vector(&embedding)?;
        Ok(PendingDocument {
            id: memory.id,
            title: memory.title,
            content_hash: hash_hex(memory.content.as_bytes()),
            content: memory.content,
            search_text,
            kind: memory.kind,
            importance: memory.importance,
            tags: memory.tags,
            source: memory.source,
            created_at_ms: memory.created_at_ms,
            updated_at_ms: memory.updated_at_ms,
            embedding,
        })
    }

    fn ensure_target_runtime(&mut self, job: &ModelSwitchJournal) -> Result<()> {
        if job.allow_dense_downtime && self.switch_target_embedder.is_none() {
            self.embedder = None;
        }
        if self.switch_target_config.is_none() {
            self.switch_target_config = Some(if job.target_generation_id == "legacy" {
                self.legacy_snapshot(&job.target_profile_id)
                    .and_then(|snapshot| snapshot.source_embedding.clone())
                    .ok_or_else(|| {
                        anyhow!("retained legacy embedding configuration is unavailable")
                    })?
            } else {
                model::embedding_config_for_profile(
                    &job.target_profile_id,
                    self.config.embedding(),
                )?
            });
        }
        if self.switch_target_embedder.is_none() {
            let config = self
                .switch_target_config
                .as_ref()
                .ok_or_else(|| anyhow!("target embedding configuration is unavailable"))?;
            self.switch_target_embedder = Some(LlamaCppEmbedder::load_verified(
                config,
                self.config.model_cache(),
                model::artifact_sha256(&job.target_profile_id),
            )?);
        }
        if self.switch_target_collection.is_none()
            && (job.target_generation_id == "legacy"
                || generation_manifest_path(&self.config, &job.target_generation_id).exists())
        {
            let config = self
                .switch_target_config
                .as_ref()
                .ok_or_else(|| anyhow!("target embedding configuration is unavailable"))?;
            let embedder = self
                .switch_target_embedder
                .as_ref()
                .ok_or_else(|| anyhow!("target embedding worker is unavailable"))?;
            let fingerprint = self
                .config
                .clone()
                .with_embedding(config.clone())
                .embedding_profile_fingerprint()?;
            self.switch_target_collection = Some(if job.target_generation_id == "legacy" {
                let target = self.config.clone().with_embedding(config.clone());
                zvec::open_existing_collection(
                    &target,
                    embedder.model_id(),
                    embedder.dimension(),
                    now_ms()?,
                )?
            } else {
                zvec::open_generation_collection(
                    &self.config,
                    &job.target_generation_id,
                    &job.target_profile_id,
                    &fingerprint,
                    embedder.model_id(),
                    embedder.dimension(),
                    now_ms()?,
                )?
            });
        }
        Ok(())
    }

    fn ensure_source_unchanged(&self, job: &ModelSwitchJournal) -> Result<()> {
        ensure!(
            (job.source_content_digest.is_empty()
                && self.state.generation == job.source_state_revision
                || !job.source_content_digest.is_empty()
                    && self.state.canonical_revision_digest()? == job.source_content_digest)
                && self.active_embedding.generation_id == job.source_generation_id
                && self.state.pending_upserts.is_empty()
                && self.state.pending_deletes.is_empty(),
            "SOURCE_CHANGED: canonical source state changed during model switch"
        );
        Ok(())
    }

    fn transition_switch(&mut self, phase: SwitchPhase) -> Result<()> {
        let current = self
            .switch_store
            .current
            .as_mut()
            .ok_or_else(|| anyhow!("model switch journal disappeared"))?;
        current.phase = phase;
        current.updated_at_ms = now_ms()?;
        self.save_switch_store()
    }

    fn finish_switch_cancelled(&mut self) -> Result<()> {
        let (generation_id, switch_id, target_preexisting) = self
            .switch_store
            .current
            .as_ref()
            .map(|job| {
                (
                    job.target_generation_id.clone(),
                    job.switch_id.clone(),
                    job.target_preexisting,
                )
            })
            .ok_or_else(|| anyhow!("model switch journal disappeared"))?;
        let manifest_path = generation_manifest_path(&self.config, &generation_id);
        if !target_preexisting && manifest_path.exists() {
            let mut manifest =
                EmbeddingGenerationManifest::load(&manifest_path, self.config.project_id())?;
            manifest.state = GenerationState::Quarantined;
            manifest.save(&manifest_path)?;
        }
        self.switch_target_collection = None;
        self.switch_target_embedder = None;
        self.switch_target_config = None;
        let now = now_ms()?;
        let current = self.current_switch_mut(&switch_id)?;
        current.phase = SwitchPhase::Cancelled;
        current.completed_at_ms = Some(now);
        current.updated_at_ms = now;
        self.save_switch_store()?;
        if self.embedder.is_none() {
            self.embedder =
                LlamaCppEmbedder::load(self.config.embedding(), self.config.model_cache()).ok();
        }
        Ok(())
    }

    fn fail_model_switch(&mut self, switch_id: &str, error: &anyhow::Error) -> Result<()> {
        if let Some(active) = ActiveEmbedding::load(
            &self.config.active_embedding_path(),
            self.config.project_id(),
        )?
        .filter(|active| {
            self.switch_store.current.as_ref().is_some_and(|job| {
                job.switch_id == switch_id && job.target_generation_id == active.generation_id
            })
        }) {
            let current = self.current_switch_mut(switch_id)?;
            current.phase = SwitchPhase::Committing;
            current.updated_at_ms = now_ms()?;
            current.error_code = Some(switch_error_code(error).to_string());
            current.error_message = Some(format!("{error:#}"));
            self.switch_target_collection = None;
            self.switch_target_embedder = None;
            self.switch_target_config = None;
            self.active_embedding = active;
            return self.save_switch_store();
        }
        let target = self
            .switch_store
            .current
            .as_ref()
            .filter(|job| job.switch_id == switch_id)
            .map(|job| (job.target_generation_id.clone(), job.target_preexisting));
        if let Some((generation_id, false)) = target {
            let path = generation_manifest_path(&self.config, &generation_id);
            if path.exists()
                && let Ok(mut manifest) =
                    EmbeddingGenerationManifest::load(&path, self.config.project_id())
            {
                manifest.state = GenerationState::Quarantined;
                let _ = manifest.save(&path);
            }
        }
        self.switch_target_collection = None;
        self.switch_target_embedder = None;
        self.switch_target_config = None;
        let now = now_ms()?;
        let current = self.current_switch_mut(switch_id)?;
        current.phase = SwitchPhase::Failed;
        current.completed_at_ms = Some(now);
        current.updated_at_ms = now;
        current.error_code = Some(switch_error_code(error).to_string());
        current.error_message = Some(format!("{error:#}"));
        self.save_switch_store()?;
        if self.embedder.is_none() {
            self.embedder =
                LlamaCppEmbedder::load(self.config.embedding(), self.config.model_cache()).ok();
        }
        Ok(())
    }

    fn current_switch_mut(&mut self, switch_id: &str) -> Result<&mut ModelSwitchJournal> {
        self.switch_store
            .current
            .as_mut()
            .filter(|job| job.switch_id == switch_id)
            .ok_or_else(|| anyhow!("model switch not found: {switch_id}"))
    }

    fn legacy_snapshot(&self, profile_id: &str) -> Option<&ModelSwitchJournal> {
        self.switch_store
            .current
            .iter()
            .chain(self.switch_store.history.iter().rev())
            .find(|job| {
                job.phase == SwitchPhase::Succeeded
                    && job.source_generation_id == "legacy"
                    && job.source_profile_id == profile_id
            })
    }

    fn save_switch_store(&self) -> Result<()> {
        self.switch_store.save(&self.config.model_switch_path())
    }

    fn switch_status(&self, job: &ModelSwitchJournal) -> ModelSwitchStatusResponse {
        ModelSwitchStatusResponse {
            switch_id: job.switch_id.clone(),
            state: phase_name(job.phase).to_string(),
            active_profile_id: self.active_embedding.profile_id.clone(),
            target_profile_id: job.target_profile_id.clone(),
            active_generation_id: self.active_embedding.generation_id.clone(),
            target_generation_id: Some(job.target_generation_id.clone()),
            completed_records: job.completed_records,
            total_records: job.total_records,
            fraction: if job.total_records == 0 {
                f64::from(job.phase == SwitchPhase::Succeeded)
            } else {
                job.completed_records as f64 / job.total_records as f64
            },
            cancel_requested: job.cancel_requested,
            dense_search_available: self.embedder.is_some(),
            created_at_ms: job.created_at_ms,
            updated_at_ms: job.updated_at_ms,
            completed_at_ms: job.completed_at_ms,
            error: job.error_code.as_ref().map(|code| ModelProfileReason {
                code: code.clone(),
                message: job.error_message.clone().unwrap_or_default(),
            }),
        }
    }

    fn switch_response(
        &self,
        job: ModelSwitchJournal,
        preflight: model::ModelSwitchPreflight,
    ) -> Result<ModelSwitchResponse> {
        Ok(ModelSwitchResponse {
            switch_id: Some(job.switch_id),
            dry_run: false,
            state: phase_name(job.phase).to_string(),
            active_profile_id: self.active_embedding.profile_id.clone(),
            target_profile_id: job.target_profile_id,
            active_generation_id: self.active_embedding.generation_id.clone(),
            target_generation_id: Some(job.target_generation_id),
            dense_search_available: self.embedder.is_some(),
            preflight,
        })
    }
}

fn build_target_document(pending: &PendingDocument, dimension: usize) -> Result<Doc> {
    ensure!(
        pending.embedding.len() == dimension,
        "target embedding dimension mismatch"
    );
    let mut doc = Doc::new()?;
    doc.set_pk(&pending.id);
    doc.add_string("title", &pending.title)?;
    doc.add_string("content", &pending.content)?;
    doc.add_string("search_text", &pending.search_text)?;
    doc.add_string("kind", pending.kind.as_str())?;
    doc.add_f32("importance", pending.importance)?;
    doc.add_string("tags", &serde_json::to_string(&pending.tags)?)?;
    doc.add_string("source", &pending.source)?;
    doc.add_string("content_hash", &pending.content_hash)?;
    doc.add_i64("created_at", pending.created_at_ms)?;
    doc.add_i64("updated_at", pending.updated_at_ms)?;
    doc.add_vector_f32("embedding", &pending.embedding)?;
    Ok(doc)
}

fn validate_target_vector(vector: &[f32]) -> Result<()> {
    ensure!(
        !vector.is_empty(),
        "TARGET_VECTOR_INVALID: target vector is empty"
    );
    ensure!(
        vector.iter().all(|value| value.is_finite()),
        "TARGET_VECTOR_INVALID: target vector contains non-finite values"
    );
    let norm = vector
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    ensure!(
        (norm - 1.0).abs() <= 0.02,
        "TARGET_VECTOR_INVALID: target vector is not normalized"
    );
    Ok(())
}

fn validate_switch_id(switch_id: &str) -> Result<()> {
    ensure!(
        !switch_id.is_empty()
            && switch_id.len() <= MAX_SWITCH_ID_BYTES
            && switch_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "model switch ID is invalid"
    );
    Ok(())
}

fn validate_generation_id(generation_id: &str) -> Result<()> {
    ensure!(
        generation_id == "legacy"
            || generation_id.starts_with("gen_")
                && generation_id.len() <= 64
                && generation_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "embedding generation ID is invalid"
    );
    Ok(())
}

fn phase_name(phase: SwitchPhase) -> &'static str {
    match phase {
        SwitchPhase::Queued => "queued",
        SwitchPhase::Validating => "validating",
        SwitchPhase::Downloading => "downloading",
        SwitchPhase::Preparing => "preparing",
        SwitchPhase::Reindexing => "reindexing",
        SwitchPhase::Verifying => "verifying",
        SwitchPhase::Committing => "committing",
        SwitchPhase::Succeeded => "succeeded",
        SwitchPhase::CancelRequested => "cancel_requested",
        SwitchPhase::Cancelled => "cancelled",
        SwitchPhase::Failed => "failed",
    }
}

fn switch_error_code(error: &anyhow::Error) -> &'static str {
    let message = error.to_string();
    for code in [
        "PROFILE_NOT_FOUND",
        "PROFILE_UNSUPPORTED",
        "ACTIVE_PROFILE_MISMATCH",
        "SWITCH_IN_PROGRESS",
        "INSUFFICIENT_DISK",
        "INSUFFICIENT_MEMORY",
        "ARTIFACT_VERIFICATION_FAILED",
        "TARGET_VECTOR_INVALID",
        "SOURCE_CHANGED",
        "CANCEL_TOO_LATE",
    ] {
        if message.contains(code) {
            return code;
        }
    }
    "MODEL_SWITCH_FAILED"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_vector_validation_rejects_wrong_norm_and_non_finite_values() {
        validate_target_vector(&[0.6, 0.8]).expect("normalized vector");
        assert!(validate_target_vector(&[1.0, 1.0]).is_err());
        assert!(validate_target_vector(&[f32::NAN]).is_err());
    }

    #[test]
    fn switch_ids_are_bounded_and_path_safe() {
        validate_switch_id("switch_0123-abcd").expect("valid ID");
        assert!(validate_switch_id("../switch").is_err());
        assert!(validate_switch_id("").is_err());
    }

    #[test]
    fn generation_ids_allow_named_and_legacy_rollback_targets() {
        validate_generation_id("gen_0123abcd").expect("named generation");
        validate_generation_id("legacy").expect("legacy generation");
        assert!(validate_generation_id("../legacy").is_err());
    }
}
