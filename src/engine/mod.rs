mod retrieval;

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail, ensure};
use zvec_rust::{Collection, Doc, SearchQuery};

use crate::MemoryConfig;
use crate::config::hash_hex;
use crate::contract::{
    DeleteReason, DeleteRequest, DeleteResponse, DoctorRequest, DoctorResponse, FeedbackEvent,
    FeedbackRequest, FeedbackResponse, ForgetRequest, ForgetResponse, GetRequest, IndexStatus,
    LifecycleResponse, ListRequest, ListResponse, LockRequest, MemoryKind, MemoryOrigin,
    MemoryRecord, MemoryScope, OptimizeResponse, PinRequest, PurgeRequest, PurgeResponse,
    StatusResponse, StoreRequest, StoreResponse, SyncSharedRequest, SyncSharedResponse,
    UpdateRequest, UpdateResponse,
};
use crate::embedding::{Embedder, LlamaCppEmbedder};
use crate::lifecycle::{
    default_expiry, default_half_life_days, ensure_delete_allowed, ensure_store_overwrite_allowed,
    expiry_from_days, is_expired, is_prunable_expired, resolve_update,
};
use crate::storage::state::{
    MemoryMetadata, MemoryState, STATE_SCHEMA_VERSION, Tombstone, memory_fingerprint,
};
use crate::storage::zvec::{self, RESULT_FIELDS, ensure_write_succeeded};
use crate::taxonomy::MemoryTaxonomy;
use crate::validation::{
    MAX_ID_COUNT, MAX_LIST_RESULTS, MAX_SHARED_RECORDS, NormalizedStoreRequest, anchors_stale,
    capture_code_anchors, git_head, normalize_scope_key, validate_ids, validate_retrieval_id,
    validate_shared_source, validate_store_request,
};

const MAX_STATE_BACKFILL_RECORDS: usize = 10_000;
const SESSION_DEFAULT_TTL_DAYS: u32 = 7;

pub struct MemoryEngine {
    config: MemoryConfig,
    collection: Collection,
    embedder: LlamaCppEmbedder,
    state: MemoryState,
    _writer_lock: File,
}

impl MemoryEngine {
    /// Open the project collection, lifecycle state, and local embedding model.
    ///
    /// # Errors
    ///
    /// Returns an error when storage cannot be locked/opened, state is
    /// incompatible, or the embedding model cannot be loaded.
    pub fn open(config: MemoryConfig) -> Result<Self> {
        zvec::initialize()?;
        zvec::secure_create_dir(&config.project_data_dir())?;
        zvec::secure_create_dir(config.model_cache())?;

        let writer_lock = zvec::acquire_writer_lock(&config.project_data_dir())?;
        let state = MemoryState::load(&config.state_path())?;
        let embedder = LlamaCppEmbedder::load(config.embedding(), config.model_cache())?;
        let collection = zvec::open_collection(
            &config,
            embedder.model_id(),
            embedder.dimension(),
            now_ms()?,
        )?;

        let mut engine = Self {
            config,
            collection,
            embedder,
            state,
            _writer_lock: writer_lock,
        };
        engine.recover_pending_deletes()?;
        engine.backfill_legacy_state()?;
        engine.enrich_migrated_metadata()?;
        Ok(engine)
    }

    /// Validate, embed, deduplicate, and durably upsert one memory.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid/sensitive input, a tombstone, a locked
    /// existing record, or a storage/inference failure.
    #[allow(clippy::too_many_lines)]
    pub fn store(&mut self, request: StoreRequest) -> Result<StoreResponse> {
        let normalized = validate_store_request(request)?;
        let fingerprint = memory_fingerprint(
            normalized.kind,
            normalized.scope,
            normalized.scope_key.as_deref(),
            &normalized.content,
        );
        if self.state.is_tombstoned(&fingerprint) && !normalized.revive {
            bail!(
                "memory was previously deleted and is tombstoned; set revive=true after user approval"
            );
        }

        let scoped_id = deterministic_memory_id(
            normalized.kind,
            normalized.scope,
            normalized.scope_key.as_deref(),
            &normalized.content,
        );
        let legacy_id = legacy_memory_id(normalized.kind, &normalized.content);
        let legacy_exists = normalized.scope == MemoryScope::Project
            && legacy_id != scoped_id
            && !self
                .collection
                .fetch_with_options(&[legacy_id.as_str()], Some(&["created_at"]), false)?
                .is_empty();
        let id = if legacy_exists { legacy_id } else { scoped_id };
        let now = now_ms()?;
        let existing =
            self.collection
                .fetch_with_options(&[id.as_str()], Some(&["created_at"]), false)?;
        let inserted = existing.is_empty();
        let created_at = existing
            .first()
            .and_then(|doc| doc.get_i64("created_at").ok().flatten())
            .unwrap_or(now);
        let existing_metadata = (!inserted).then(|| {
            self.state
                .metadata(&id, normalized.kind, created_at, normalized.importance)
        });
        if let Some(metadata) = &existing_metadata {
            ensure_store_overwrite_allowed(metadata)?;
        }

        let code_anchors = capture_code_anchors(&self.config, &normalized.code_paths)?;
        let expires_at_ms = initial_expiry(&normalized, created_at, now);
        let metadata = MemoryMetadata {
            scope: normalized.scope,
            scope_key: normalized.scope_key.clone(),
            origin: normalized.origin,
            expires_at_ms,
            half_life_days: default_half_life_days(normalized.kind),
            code_anchors: code_anchors.clone(),
            feedback: existing_metadata
                .as_ref()
                .map(|item| item.feedback.clone())
                .unwrap_or_default(),
            shared_source: if normalized.origin == MemoryOrigin::SharedMarkdown {
                normalized
                    .source
                    .strip_prefix("shared:")
                    .map(ToOwned::to_owned)
            } else {
                None
            },
            pinned: existing_metadata
                .as_ref()
                .is_some_and(|metadata| metadata.pinned),
            locked: false,
            lock_reason: None,
            taxonomy: normalized.taxonomy,
            confidence: normalized.confidence,
            superseded_by: None,
            supersedes: existing_metadata
                .as_ref()
                .map(|m| m.supersedes.clone())
                .unwrap_or_default(),
            conflict_with: existing_metadata
                .as_ref()
                .map(|m| m.conflict_with.clone())
                .unwrap_or_default(),
        };
        let previous_metadata = self.state.records.insert(id.clone(), metadata);
        self.save_state()?;
        let content_hash = match self.write_document(&id, &normalized, created_at, now) {
            Ok(content_hash) => content_hash,
            Err(error) => {
                if let Some(previous) = previous_metadata {
                    self.state.records.insert(id.clone(), previous);
                } else {
                    self.state.records.remove(&id);
                }
                let _ = self.save_state();
                return Err(error);
            }
        };
        if normalized.revive {
            self.state.tombstones.remove(&fingerprint);
            self.save_state()?;
        }

        Ok(StoreResponse {
            id,
            inserted,
            content_hash,
            updated_at_ms: now,
            scope: normalized.scope,
        })
    }

    /// Fetch complete memories in the same order as the requested IDs.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid IDs, corrupt records, or a storage failure.
    pub fn get(&self, request: &GetRequest) -> Result<Vec<MemoryRecord>> {
        validate_ids(&request.ids)?;
        let documents = self.fetch_documents(&request.ids)?;
        let mut by_id = HashMap::new();
        for document in &documents {
            let stored = stored_memory_from_doc(document)?;
            if self.state.pending_deletes.contains(&stored.id) {
                continue;
            }
            let metadata = self.state.metadata(
                &stored.id,
                stored.kind,
                stored.created_at_ms,
                stored.importance,
            );
            if !management_visible(
                &metadata,
                request.session_scope_key.as_deref(),
                request.agent_scope_key.as_deref(),
            ) {
                continue;
            }
            let stale = anchors_stale(&self.config, &metadata.code_anchors);
            by_id.insert(stored.id.clone(), decorate_memory(stored, metadata, stale));
        }
        Ok(request
            .ids
            .iter()
            .filter_map(|id| by_id.remove(id))
            .collect())
    }

    /// List lifecycle-indexed memories for human management.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid pagination or corrupt records.
    pub fn list(&self, request: &ListRequest) -> Result<ListResponse> {
        ensure!(
            (1..=MAX_LIST_RESULTS).contains(&request.limit),
            "list limit must be between 1 and {MAX_LIST_RESULTS}"
        );
        let now = now_ms()?;
        let mut ids = self.state.records.keys().cloned().collect::<Vec<_>>();
        ids.sort();
        let documents = self.fetch_documents(&ids)?;
        let mut memories = Vec::new();
        for document in &documents {
            let stored = stored_memory_from_doc(document)?;
            if self.state.pending_deletes.contains(&stored.id) {
                continue;
            }
            let metadata = self.state.metadata(
                &stored.id,
                stored.kind,
                stored.created_at_ms,
                stored.importance,
            );
            if !management_visible(
                &metadata,
                request.session_scope_key.as_deref(),
                request.agent_scope_key.as_deref(),
            ) {
                continue;
            }
            if !request.kinds.is_empty() && !request.kinds.contains(&stored.kind) {
                continue;
            }
            if !request.scopes.is_empty() && !request.scopes.contains(&metadata.scope) {
                continue;
            }
            if !request.taxonomies.is_empty() && !request.taxonomies.contains(&metadata.taxonomy) {
                continue;
            }
            if !request.include_superseded && metadata.is_superseded() {
                continue;
            }
            if !request.include_expired && is_expired(&metadata, now) {
                continue;
            }
            let stale = anchors_stale(&self.config, &metadata.code_anchors);
            if stale && !request.include_stale {
                continue;
            }
            memories.push(decorate_memory(stored, metadata, stale));
        }
        memories.sort_by_key(|memory| Reverse(memory.updated_at_ms));
        let total = memories.len();
        let page = memories
            .into_iter()
            .skip(request.offset)
            .take(request.limit)
            .collect::<Vec<_>>();
        Ok(ListResponse {
            total,
            offset: request.offset,
            count: page.len(),
            memories: page,
        })
    }

    /// Update a memory by stable ID with optimistic concurrency.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid changes, lifecycle violations, stale
    /// timestamps, or missing IDs.
    #[allow(clippy::too_many_lines)]
    pub fn update(&mut self, request: UpdateRequest) -> Result<UpdateResponse> {
        validate_ids(std::slice::from_ref(&request.id))?;
        let documents = self.fetch_documents(std::slice::from_ref(&request.id))?;
        let document = documents
            .first()
            .ok_or_else(|| anyhow!("memory not found: {}", request.id))?;
        let existing = stored_memory_from_doc(document)?;
        if let Some(expected) = request.expected_updated_at_ms {
            ensure!(
                expected == existing.updated_at_ms,
                "memory changed since it was read; fetch it again before updating"
            );
        }
        let old_metadata = self.state.metadata(
            &existing.id,
            existing.kind,
            existing.created_at_ms,
            existing.importance,
        );
        ensure!(
            !old_metadata.is_superseded(),
            "cannot update superseded historical memory; update its active successor"
        );
        ensure!(
            management_visible(
                &old_metadata,
                request.session_scope_key.as_deref(),
                request.agent_scope_key.as_deref(),
            ),
            "memory is not visible to the current session or agent"
        );
        ensure!(
            old_metadata.scope != MemoryScope::Repository,
            "repository memory must be updated through its Markdown source"
        );
        let scope = request.scope.unwrap_or(old_metadata.scope);
        let lifecycle = resolve_update(&old_metadata, &request, scope)?;
        let scope_key = if request.scope.is_some() || request.scope_key.is_some() {
            normalize_scope_key(scope, request.scope_key.as_deref())?
        } else {
            old_metadata.scope_key.clone()
        };
        let code_paths = request.code_paths.clone().unwrap_or_default();
        let merged = validate_store_request(StoreRequest {
            content: request.content.unwrap_or_else(|| existing.content.clone()),
            title: Some(request.title.unwrap_or_else(|| existing.title.clone())),
            kind: request.kind.unwrap_or(existing.kind),
            importance: request.importance.unwrap_or(existing.importance),
            tags: request.tags.unwrap_or_else(|| existing.tags.clone()),
            source: Some(existing.source.clone()),
            scope,
            scope_key: scope_key.clone(),
            origin: old_metadata.origin,
            expires_in_days: request.expires_in_days,
            code_paths,
            revive: false,
            taxonomy: request.taxonomy.or(Some(old_metadata.taxonomy)),
            confidence: request.confidence.or(Some(old_metadata.confidence)),
        })?;
        let new_fingerprint = memory_fingerprint(
            merged.kind,
            merged.scope,
            merged.scope_key.as_deref(),
            &merged.content,
        );
        ensure!(
            !self.state.is_tombstoned(&new_fingerprint),
            "updated content matches a tombstoned memory"
        );
        let now = now_ms()?;
        let code_anchors = if request.code_paths.is_some() {
            capture_code_anchors(&self.config, &merged.code_paths)?
        } else {
            old_metadata.code_anchors.clone()
        };
        let expires_at_ms = if request.clear_expiry {
            None
        } else if let Some(days) = request.expires_in_days {
            expiry_from_days(now, Some(days))
        } else {
            old_metadata.expires_at_ms
        };
        let metadata = MemoryMetadata {
            scope: merged.scope,
            scope_key,
            origin: old_metadata.origin,
            expires_at_ms,
            half_life_days: default_half_life_days(merged.kind),
            code_anchors,
            feedback: old_metadata.feedback,
            shared_source: old_metadata.shared_source,
            pinned: lifecycle.pinned,
            locked: lifecycle.locked,
            lock_reason: lifecycle.lock_reason,
            taxonomy: merged.taxonomy,
            confidence: merged.confidence,
            superseded_by: None,
            supersedes: old_metadata.supersedes.clone(),
            conflict_with: old_metadata.conflict_with.clone(),
        };
        self.commit_update(
            &request.id,
            existing.created_at_ms,
            &merged,
            metadata,
            request.conflict_with,
            now,
        )
    }

    /// Phase 1 dedicated pin RPC sharing `update`'s optimistic-concurrency
    /// and scope-authorization semantics.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid IDs, locked records, repository scope, or
    /// stale timestamps.
    pub fn pin(&mut self, request: &PinRequest) -> Result<LifecycleResponse> {
        self.update_lifecycle(UpdateRequest {
            id: request.id.clone(),
            expected_updated_at_ms: request.expected_updated_at_ms,
            content: None,
            title: None,
            kind: None,
            importance: None,
            tags: None,
            scope: None,
            scope_key: None,
            expires_in_days: None,
            clear_expiry: false,
            code_paths: None,
            pinned: Some(request.pinned),
            lock_action: None,
            lock_reason: None,
            taxonomy: None,
            confidence: None,
            conflict_with: None,
            session_scope_key: request.session_scope_key.clone(),
            agent_scope_key: request.agent_scope_key.clone(),
        })
    }

    /// Phase 1 dedicated lock RPC sharing `update`'s optimistic-concurrency
    /// and scope-authorization semantics.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid IDs, repository scope, invalid lock
    /// reason, or stale timestamps.
    pub fn lock(&mut self, request: &LockRequest) -> Result<LifecycleResponse> {
        self.update_lifecycle(UpdateRequest {
            id: request.id.clone(),
            expected_updated_at_ms: request.expected_updated_at_ms,
            content: None,
            title: None,
            kind: None,
            importance: None,
            tags: None,
            scope: None,
            scope_key: None,
            expires_in_days: None,
            clear_expiry: false,
            code_paths: None,
            pinned: None,
            lock_action: Some(request.lock_action),
            lock_reason: request.lock_reason.clone(),
            taxonomy: None,
            confidence: None,
            conflict_with: None,
            session_scope_key: request.session_scope_key.clone(),
            agent_scope_key: request.agent_scope_key.clone(),
        })
    }

    fn update_lifecycle(&mut self, request: UpdateRequest) -> Result<LifecycleResponse> {
        validate_ids(std::slice::from_ref(&request.id))?;
        let documents = self.fetch_documents(std::slice::from_ref(&request.id))?;
        let document = documents
            .first()
            .ok_or_else(|| anyhow!("memory not found: {}", request.id))?;
        let existing = stored_memory_from_doc(document)?;
        if let Some(expected) = request.expected_updated_at_ms {
            ensure!(
                expected == existing.updated_at_ms,
                "memory changed since it was read; fetch it again before updating"
            );
        }
        let mut metadata = self.state.metadata(
            &existing.id,
            existing.kind,
            existing.created_at_ms,
            existing.importance,
        );
        ensure!(
            !metadata.is_superseded(),
            "cannot change lifecycle state on superseded historical memory"
        );
        ensure!(
            management_visible(
                &metadata,
                request.session_scope_key.as_deref(),
                request.agent_scope_key.as_deref(),
            ),
            "memory is not visible to the current session or agent"
        );
        ensure!(
            metadata.scope != MemoryScope::Repository,
            "repository memory lifecycle is managed through its Markdown source"
        );
        let values = resolve_update(&metadata, &request, metadata.scope)?;
        let previous = metadata.clone();
        metadata.pinned = values.pinned;
        metadata.locked = values.locked;
        metadata.lock_reason = values.lock_reason;
        self.state
            .records
            .insert(existing.id.clone(), metadata.clone());
        if let Err(error) = self.save_state() {
            self.state.records.insert(existing.id.clone(), previous);
            return Err(error);
        }
        Ok(LifecycleResponse {
            id: existing.id,
            pinned: metadata.pinned,
            locked: metadata.locked,
            lock_reason: metadata.lock_reason,
            updated_at_ms: existing.updated_at_ms,
        })
    }

    /// Delete memories, optionally leaving tombstones that block relearning.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid IDs, locked records, or a storage failure.
    pub fn delete(&mut self, request: &DeleteRequest) -> Result<DeleteResponse> {
        validate_ids(&request.ids)?;
        self.ensure_management_access(
            &request.ids,
            request.session_scope_key.as_deref(),
            request.agent_scope_key.as_deref(),
        )?;
        self.delete_internal(&request.ids, request.tombstone, request.reason)
    }

    /// Backward-compatible delete alias that always leaves tombstones.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid IDs, locked records, or a storage failure.
    pub fn forget(&mut self, request: &ForgetRequest) -> Result<ForgetResponse> {
        self.delete(&DeleteRequest {
            ids: request.ids.clone(),
            tombstone: true,
            reason: DeleteReason::UserDeleted,
            session_scope_key: request.session_scope_key.clone(),
            agent_scope_key: request.agent_scope_key.clone(),
        })
    }

    /// Purge all indexed records after verifying the current project ID.
    ///
    /// This explicit store-wide operation bypasses per-record locks.
    ///
    /// # Errors
    ///
    /// Returns an error for a project mismatch or storage failure.
    pub fn purge(&mut self, request: &PurgeRequest) -> Result<PurgeResponse> {
        ensure!(
            request.project_id == self.config.project_id(),
            "project id confirmation does not match the active memory store"
        );
        let deleted = self.collection.stats()?.doc_count;
        if deleted > 0 {
            self.collection.delete_by_filter("created_at >= 0")?;
            self.collection.flush()?;
        }
        self.state.records.clear();
        self.state.retrievals.clear();
        self.state.pending_deletes.clear();
        if !request.keep_tombstones {
            self.state.tombstones.clear();
        }
        self.save_state()?;
        Ok(PurgeResponse {
            deleted,
            tombstones_retained: self.state.tombstones.len(),
        })
    }

    /// Record explicit or proxy retrieval feedback idempotently.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown retrieval or invalid memory IDs.
    pub fn feedback(&mut self, request: &FeedbackRequest) -> Result<FeedbackResponse> {
        validate_retrieval_id(&request.retrieval_id)?;
        if !request.memory_ids.is_empty() {
            validate_ids(&request.memory_ids)?;
        }
        let retrieval = self
            .state
            .retrievals
            .get(&request.retrieval_id)
            .ok_or_else(|| anyhow!("unknown retrieval id: {}", request.retrieval_id))?;
        let event_key = feedback_event_key(request.event);
        if retrieval.events.contains(&request.event) {
            return Ok(FeedbackResponse {
                retrieval_id: request.retrieval_id.clone(),
                recorded: false,
                affected: 0,
            });
        }
        let mut requested = if request.memory_ids.is_empty() {
            retrieval.memory_ids.clone()
        } else {
            request
                .memory_ids
                .iter()
                .filter(|id| retrieval.memory_ids.contains(id))
                .cloned()
                .collect::<Vec<_>>()
        };
        ensure!(
            request.memory_ids.is_empty() || !requested.is_empty(),
            "feedback memory_ids do not belong to this retrieval"
        );
        let already_recorded = retrieval
            .event_memory_ids
            .get(event_key)
            .cloned()
            .unwrap_or_default();
        if request.event == FeedbackEvent::Ignored {
            let used = retrieval
                .event_memory_ids
                .get(feedback_event_key(FeedbackEvent::Used))
                .cloned()
                .unwrap_or_default();
            requested.retain(|id| !used.contains(id));
        }
        requested.retain(|id| !already_recorded.contains(id));
        if requested.is_empty() {
            return Ok(FeedbackResponse {
                retrieval_id: request.retrieval_id.clone(),
                recorded: false,
                affected: 0,
            });
        }
        let mut affected = 0;
        for id in &requested {
            let Some(metadata) = self.state.records.get_mut(id) else {
                continue;
            };
            match request.event {
                FeedbackEvent::Injected => {
                    metadata.feedback.injected = metadata.feedback.injected.saturating_add(1);
                }
                FeedbackEvent::Used => {
                    metadata.feedback.used = metadata.feedback.used.saturating_add(1);
                }
                FeedbackEvent::Ignored => {
                    metadata.feedback.ignored = metadata.feedback.ignored.saturating_add(1);
                }
                FeedbackEvent::Error => {
                    metadata.feedback.error = metadata.feedback.error.saturating_add(1);
                }
            }
            affected += 1;
        }
        let retrieval = self
            .state
            .retrievals
            .get_mut(&request.retrieval_id)
            .ok_or_else(|| anyhow!("unknown retrieval id: {}", request.retrieval_id))?;
        let recorded = retrieval
            .event_memory_ids
            .entry(event_key.to_string())
            .or_default();
        recorded.extend(requested);
        recorded.sort();
        recorded.dedup();
        self.save_state()?;
        Ok(FeedbackResponse {
            retrieval_id: request.retrieval_id.clone(),
            recorded: true,
            affected,
        })
    }

    /// Synchronize approved repository Markdown into the local search index.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is oversized or storage fails.
    pub fn sync_shared(&mut self, request: SyncSharedRequest) -> Result<SyncSharedResponse> {
        ensure!(
            request.records.len() <= MAX_SHARED_RECORDS,
            "at most {MAX_SHARED_RECORDS} shared memories are allowed"
        );
        let mut incoming_sources = HashSet::new();
        for record in &request.records {
            validate_shared_source(&record.source)?;
            ensure!(
                incoming_sources.insert(record.source.clone()),
                "duplicate shared memory source: {}",
                record.source
            );
        }
        let existing = self
            .state
            .records
            .iter()
            .filter_map(|(id, metadata)| {
                if metadata.origin != MemoryOrigin::SharedMarkdown {
                    return None;
                }
                metadata
                    .shared_source
                    .as_ref()
                    .map(|source| (source.clone(), id.clone()))
            })
            .collect::<HashMap<_, _>>();
        let mut removed = 0;
        for (source, id) in &existing {
            if !incoming_sources.contains(source) {
                removed += usize::try_from(
                    self.delete_internal(std::slice::from_ref(id), false, DeleteReason::Obsolete)?
                        .deleted,
                )?;
            }
        }

        let mut imported = 0;
        let mut rejected_sources = Vec::new();
        for record in request.records {
            let shared_source = record.source.clone();
            let stored = self.store(StoreRequest {
                content: record.content,
                title: Some(record.title),
                kind: record.kind,
                importance: record.importance,
                tags: record.tags,
                source: Some(format!("shared:{}", record.source)),
                scope: MemoryScope::Repository,
                scope_key: None,
                origin: MemoryOrigin::SharedMarkdown,
                expires_in_days: None,
                code_paths: record.code_paths,
                revive: false,
                taxonomy: None,
                confidence: None,
            });
            match stored {
                Ok(stored) => {
                    if let Some(old_id) = existing.get(&shared_source)
                        && old_id != &stored.id
                    {
                        removed += usize::try_from(
                            self.delete_internal(
                                std::slice::from_ref(old_id),
                                false,
                                DeleteReason::Obsolete,
                            )?
                            .deleted,
                        )?;
                    }
                    imported += 1;
                }
                Err(_) => rejected_sources.push(shared_source),
            }
        }
        let rejected = rejected_sources.len();
        Ok(SyncSharedResponse {
            imported,
            removed,
            rejected,
            rejected_sources,
        })
    }

    /// Report collection, model, lifecycle state, and project status.
    ///
    /// # Errors
    ///
    /// Returns an error when collection statistics cannot be read.
    pub fn status(&self) -> Result<StatusResponse> {
        let stats = self.collection.stats()?;
        Ok(StatusResponse {
            ready: true,
            rpc_protocol_version: crate::rpc::RPC_PROTOCOL_VERSION,
            backend: "zvec+llama.cpp".to_string(),
            zvec_version: zvec_rust::version().clone(),
            embedding_model: self.embedder.model_id().to_string(),
            embedding_dimension: self.embedder.dimension(),
            project_root: self.config.project_root().display().to_string(),
            project_id: self.config.project_id().to_string(),
            collection_path: self.config.collection_dir().display().to_string(),
            document_count: stats.doc_count,
            state_schema_version: STATE_SCHEMA_VERSION,
            metadata_count: self.state.records.len(),
            tombstone_count: self.state.tombstones.len(),
            retrieval_count: self.state.retrievals.len(),
            indexes: stats
                .indexes
                .into_iter()
                .map(|index| IndexStatus {
                    name: index.name,
                    completeness: index.completeness,
                })
                .collect(),
            capabilities: vec![
                "phase1_taxonomy_lifecycle_v1",
                "llama_cpp_gguf_embeddings_v1",
                "protobuf_framed_rpc_v1",
            ],
        })
    }

    /// Prune eligible expired lifecycle state, compact segments, rebuild
    /// indexes, and flush. Pinned and locked records are retained.
    ///
    /// # Errors
    ///
    /// Returns an error when deletion, optimization, or statistics fail.
    pub fn optimize(&mut self) -> Result<OptimizeResponse> {
        let now = now_ms()?;
        let expired_ids = self
            .state
            .records
            .iter()
            .filter(|(_, metadata)| is_prunable_expired(metadata, now))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let pruned_expired = if expired_ids.is_empty() {
            0
        } else {
            usize::try_from(
                self.delete_internal(&expired_ids, false, DeleteReason::Obsolete)?
                    .deleted,
            )?
        };
        let pruned_retrievals = self.state.prune_retrievals(now);
        self.collection.optimize()?;
        self.collection.flush()?;
        self.save_state()?;
        Ok(OptimizeResponse {
            optimized: true,
            document_count: self.collection.stats()?.doc_count,
            pruned_expired,
            pruned_retrievals,
        })
    }

    /// Diagnose lifecycle state and code anchors without repairing data.
    ///
    /// # Errors
    ///
    /// Returns an error when collection statistics cannot be read.
    pub fn doctor(&self, request: &DoctorRequest) -> Result<DoctorResponse> {
        let stats = self.collection.stats()?;
        let now = now_ms()?;
        let expired_count = self
            .state
            .records
            .values()
            .filter(|metadata| is_expired(metadata, now))
            .count();
        let stale_count = if request.deep {
            self.state
                .records
                .values()
                .filter(|metadata| anchors_stale(&self.config, &metadata.code_anchors))
                .count()
        } else {
            0
        };
        let mut warnings = Vec::new();
        if u64::try_from(self.state.records.len()).unwrap_or(u64::MAX) < stats.doc_count {
            warnings.push(
                "some legacy zvec records have no lifecycle metadata until they are recalled"
                    .to_string(),
            );
        }
        if !self.config.model_cache().exists() {
            warnings.push("embedding model cache is missing".to_string());
        }
        if expired_count > 0 {
            warnings.push(format!(
                "{expired_count} expired memories await optimize or explicit lifecycle action"
            ));
        }
        if stale_count > 0 {
            warnings.push(format!("{stale_count} memories have stale code anchors"));
        }
        for index in &stats.indexes {
            if index.completeness < 1.0 {
                warnings.push(format!(
                    "index {} is only {:.1}% complete",
                    index.name,
                    index.completeness * 100.0
                ));
            }
        }
        Ok(DoctorResponse {
            ok: warnings.is_empty(),
            project_root: self.config.project_root().display().to_string(),
            project_id: self.config.project_id().to_string(),
            collection_path: self.config.collection_dir().display().to_string(),
            state_path: self.config.state_path().display().to_string(),
            model_cache: self.config.model_cache().display().to_string(),
            document_count: stats.doc_count,
            metadata_count: self.state.records.len(),
            stale_count,
            expired_count,
            tombstone_count: self.state.tombstones.len(),
            retrieval_count: self.state.retrievals.len(),
            git_sha: git_head(self.config.project_root()),
            warnings,
        })
    }

    fn recover_pending_deletes(&mut self) -> Result<()> {
        if self.state.pending_deletes.is_empty() {
            return Ok(());
        }
        let ids = self
            .state
            .pending_deletes
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let documents = self.fetch_documents(&ids)?;
        let found_ids = documents
            .iter()
            .filter_map(Doc::get_pk)
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if !found_ids.is_empty() {
            let id_refs = found_ids.iter().map(String::as_str).collect::<Vec<_>>();
            let write = self.collection.delete(&id_refs)?;
            ensure_write_succeeded("recover pending memory deletion", &write)?;
            self.collection.flush()?;
        }
        for id in ids {
            self.remove_state_record(&id);
            self.state.pending_deletes.remove(&id);
        }
        self.save_state()
    }

    fn backfill_legacy_state(&mut self) -> Result<()> {
        let stats = self.collection.stats()?;
        if stats.doc_count == 0
            || u64::try_from(self.state.records.len()).unwrap_or(u64::MAX) >= stats.doc_count
        {
            return Ok(());
        }
        let topk = usize::try_from(stats.doc_count)
            .unwrap_or(MAX_STATE_BACKFILL_RECORDS)
            .min(MAX_STATE_BACKFILL_RECORDS);
        let embedding_dimension = self.embedder.dimension();
        let dimension = embedding_dimension as f32;
        let component = 1.0 / dimension.sqrt();
        let vector = vec![component; embedding_dimension];
        let mut query = SearchQuery::new("embedding", &vector, i32::try_from(topk)?)?;
        query.set_output_fields(&RESULT_FIELDS)?;
        let mut changed = false;
        for document in self.collection.query(&query)? {
            let stored = stored_memory_from_doc(&document)?;
            if self.state.pending_deletes.contains(&stored.id) {
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.state.records.entry(stored.id)
            {
                entry.insert(MemoryMetadata::legacy(
                    stored.kind,
                    stored.created_at_ms,
                    stored.importance,
                ));
                changed = true;
            }
        }
        if changed {
            self.save_state()?;
        }
        Ok(())
    }

    fn write_document(
        &mut self,
        id: &str,
        normalized: &NormalizedStoreRequest,
        created_at: i64,
        updated_at: i64,
    ) -> Result<String> {
        let content_hash = hash_hex(normalized.content.as_bytes());
        let search_text = build_search_text(
            &normalized.title,
            normalized.kind,
            &normalized.tags,
            &normalized.content,
        );
        let embedding = self.embedder.embed_passage(&search_text)?;
        let tags_json = serde_json::to_string(&normalized.tags)?;

        let mut doc = Doc::new()?;
        doc.set_pk(id);
        doc.add_string("title", &normalized.title)?;
        doc.add_string("content", &normalized.content)?;
        doc.add_string("search_text", &search_text)?;
        doc.add_string("kind", normalized.kind.as_str())?;
        doc.add_f32("importance", normalized.importance)?;
        doc.add_string("tags", &tags_json)?;
        doc.add_string("source", &normalized.source)?;
        doc.add_string("content_hash", &content_hash)?;
        doc.add_i64("created_at", created_at)?;
        doc.add_i64("updated_at", updated_at)?;
        doc.add_vector_f32("embedding", &embedding)?;

        let write = self.collection.upsert(&[&doc])?;
        ensure_write_succeeded("store memory", &write)?;
        self.collection.flush()?;
        Ok(content_hash)
    }

    fn commit_update(
        &mut self,
        previous_id: &str,
        created_at_ms: i64,
        merged: &NormalizedStoreRequest,
        mut metadata: MemoryMetadata,
        conflict_with: Option<Vec<String>>,
        now: i64,
    ) -> Result<UpdateResponse> {
        let target_id = deterministic_memory_id(
            merged.kind,
            merged.scope,
            merged.scope_key.as_deref(),
            &merged.content,
        );
        let identity_changed = target_id != previous_id;
        if identity_changed {
            ensure!(
                self.collection
                    .fetch_with_options(&[target_id.as_str()], Some(&["created_at"]), false)?
                    .is_empty(),
                "the updated identity already exists as memory {target_id}"
            );
            // New successor directly supersedes the previous record only.
            metadata.supersedes = vec![previous_id.to_string()];
            metadata.superseded_by = None;
        }
        let inherited_conflicts = identity_changed.then(|| metadata.conflict_with.clone());

        let previous_records = self.state.records.clone();
        let previous_generation = self.state.generation;
        self.state.records.insert(target_id.clone(), metadata);
        if identity_changed {
            let predecessor =
                self.state.records.get_mut(previous_id).ok_or_else(|| {
                    anyhow!("memory lifecycle metadata is missing: {previous_id}")
                })?;
            predecessor.superseded_by = Some(target_id.clone());
        }
        if let Some(conflicts) = conflict_with.or(inherited_conflicts)
            && let Err(error) = self.apply_conflict_update_in_memory(&target_id, conflicts)
        {
            self.state.records = previous_records;
            return Err(error);
        }
        if let Err(error) = self.save_state() {
            self.state.records = previous_records;
            self.state.generation = previous_generation;
            return Err(error);
        }
        if let Err(error) = self.write_document(&target_id, merged, created_at_ms, now) {
            self.state.records = previous_records;
            self.state.generation = previous_generation;
            let _ = self.save_state();
            return Err(error);
        }

        let superseded_id = identity_changed.then(|| previous_id.to_string());
        Ok(UpdateResponse {
            id: target_id,
            previous_id: superseded_id,
            updated_at_ms: now,
        })
    }

    fn apply_conflict_update_in_memory(
        &mut self,
        id: &str,
        new_conflicts: Vec<String>,
    ) -> Result<()> {
        let metadata = self
            .state
            .records
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("memory not found: {id}"))?;
        ensure!(
            metadata.scope != MemoryScope::Repository,
            "repository memory conflict_with cannot be changed by RPC"
        );
        ensure!(
            !metadata.locked,
            "locked memory conflict_with cannot be changed"
        );
        let old_conflicts: HashSet<String> = metadata.conflict_with.iter().cloned().collect();
        let new_set: HashSet<String> = new_conflicts.iter().cloned().collect();

        // Validate all new conflict targets exist and are not self-references.
        for cid in &new_conflicts {
            ensure!(cid != id, "memory cannot conflict with itself");
            ensure!(
                self.state.records.contains_key(cid),
                "conflict target does not exist: {cid}"
            );
        }

        // Remove symmetric links for IDs no longer in the conflict set.
        for old_id in old_conflicts.difference(&new_set) {
            if let Some(other) = self.state.records.get_mut(old_id) {
                other.conflict_with.retain(|entry| entry != id);
            }
        }

        // Add symmetric links for new IDs.
        for new_id in &new_set {
            if let Some(other) = self.state.records.get_mut(new_id)
                && !other.conflict_with.contains(&id.to_string())
            {
                other.conflict_with.push(id.to_string());
            }
        }

        // Update the record's own conflict_with and cap confidence for all
        // participating records (both old and new). Clearing does not restore
        // confidence, so we only cap — never restore.
        if let Some(record) = self.state.records.get_mut(id) {
            record.conflict_with = new_conflicts;
            record.confidence = record.confidence.min(0.5);
        }
        for cid in new_set {
            if let Some(other) = self.state.records.get_mut(&cid) {
                other.confidence = other.confidence.min(0.5);
            }
        }
        Ok(())
    }

    /// Phase 1 engine migration step: enrich v1/v2-migrated records from the
    /// zvec collection (kind + importance) so that taxonomy and confidence
    /// reflect the actual stored document rather than migration defaults.
    fn enrich_migrated_metadata(&mut self) -> Result<()> {
        let Some(from_version) = self.state.schema_migrated_from else {
            return Ok(());
        };
        let ids: Vec<String> = self.state.records.keys().cloned().collect();
        let documents = self.fetch_documents(&ids)?;
        for document in &documents {
            let stored = stored_memory_from_doc(document)?;
            if let Some(metadata) = self.state.records.get_mut(&stored.id) {
                let needs_taxonomy = matches!(from_version, 1 | 2)
                    && metadata.taxonomy == MemoryTaxonomy::SessionSummary
                    && stored.kind != MemoryKind::Summary;
                if needs_taxonomy {
                    metadata.taxonomy = MemoryTaxonomy::infer_anchored(
                        stored.kind,
                        metadata.scope,
                        !metadata.code_anchors.is_empty(),
                    );
                }
                if matches!(from_version, 1 | 2) {
                    let target_confidence = if metadata.origin == MemoryOrigin::AutoCompaction {
                        stored.importance.min(0.6)
                    } else {
                        stored.importance.clamp(0.0, 1.0)
                    };
                    if (metadata.confidence - target_confidence).abs() > f32::EPSILON {
                        metadata.confidence = target_confidence;
                    }
                }
            }
        }
        self.state.schema_migrated_from = None;
        self.save_state()?;
        Ok(())
    }

    fn ensure_management_access(
        &self,
        ids: &[String],
        session_scope_key: Option<&str>,
        agent_scope_key: Option<&str>,
    ) -> Result<()> {
        for document in &self.fetch_documents(ids)? {
            let stored = stored_memory_from_doc(document)?;
            let metadata = self.state.metadata(
                &stored.id,
                stored.kind,
                stored.created_at_ms,
                stored.importance,
            );
            ensure!(
                management_visible(&metadata, session_scope_key, agent_scope_key),
                "memory {} is not visible to the current session or agent",
                stored.id
            );
        }
        Ok(())
    }

    fn delete_internal(
        &mut self,
        ids: &[String],
        create_tombstones: bool,
        reason: DeleteReason,
    ) -> Result<DeleteResponse> {
        validate_ids(ids)?;
        let documents = self.fetch_documents(ids)?;
        for document in &documents {
            let stored = stored_memory_from_doc(document)?;
            let metadata = self.state.metadata(
                &stored.id,
                stored.kind,
                stored.created_at_ms,
                stored.importance,
            );
            ensure_delete_allowed(&metadata)?;
        }

        let now = now_ms()?;
        let mut tombstones_created = 0;
        let mut found_ids = Vec::with_capacity(documents.len());
        for document in &documents {
            let stored = stored_memory_from_doc(document)?;
            found_ids.push(stored.id.clone());
            let metadata = self.state.metadata(
                &stored.id,
                stored.kind,
                stored.created_at_ms,
                stored.importance,
            );
            if create_tombstones {
                let fingerprint = memory_fingerprint(
                    stored.kind,
                    metadata.scope,
                    metadata.scope_key.as_deref(),
                    &stored.content,
                );
                self.state.add_tombstone(Tombstone {
                    fingerprint,
                    kind: stored.kind,
                    scope: metadata.scope,
                    scope_key: metadata.scope_key,
                    deleted_at_ms: now,
                    reason,
                });
                tombstones_created += 1;
            }
            self.state.pending_deletes.insert(stored.id);
        }
        self.save_state()?;
        if found_ids.is_empty() {
            return Ok(DeleteResponse {
                requested: ids.len(),
                deleted: 0,
                tombstones_created: 0,
            });
        }
        let id_refs = found_ids.iter().map(String::as_str).collect::<Vec<_>>();
        let write = self.collection.delete(&id_refs)?;
        ensure_write_succeeded("delete memory", &write)?;
        self.collection.flush()?;
        for id in &found_ids {
            self.remove_state_record(id);
            self.state.pending_deletes.remove(id);
        }
        self.save_state()?;
        Ok(DeleteResponse {
            requested: ids.len(),
            deleted: write.success_count,
            tombstones_created,
        })
    }

    fn fetch_documents(&self, ids: &[String]) -> Result<Vec<Doc>> {
        let mut documents = Vec::new();
        for chunk in ids.chunks(MAX_ID_COUNT) {
            let id_refs = chunk.iter().map(String::as_str).collect::<Vec<_>>();
            documents.extend(self.collection.fetch_with_options(
                &id_refs,
                Some(&RESULT_FIELDS),
                false,
            )?);
        }
        Ok(documents)
    }

    fn remove_state_record(&mut self, id: &str) {
        let Some(deleted) = self.state.records.get(id).cloned() else {
            return;
        };
        for conflict_id in &deleted.conflict_with {
            if let Some(other) = self.state.records.get_mut(conflict_id) {
                other.conflict_with.retain(|entry| entry != id);
            }
        }
        let successor_id = deleted.superseded_by.clone();
        for predecessor_id in &deleted.supersedes {
            if let Some(predecessor) = self.state.records.get_mut(predecessor_id)
                && predecessor.superseded_by.as_deref() == Some(id)
            {
                predecessor.superseded_by.clone_from(&successor_id);
            }
        }
        if let Some(successor_id) = successor_id
            && let Some(successor) = self.state.records.get_mut(&successor_id)
        {
            successor.supersedes.retain(|entry| entry != id);
            for predecessor_id in deleted.supersedes {
                if !successor.supersedes.contains(&predecessor_id) {
                    successor.supersedes.push(predecessor_id);
                }
            }
        }
        self.state.records.remove(id);
    }

    fn save_state(&mut self) -> Result<()> {
        self.state.save(&self.config.state_path())
    }
}

#[derive(Clone)]
struct StoredMemory {
    id: String,
    title: String,
    content: String,
    kind: MemoryKind,
    importance: f32,
    tags: Vec<String>,
    source: String,
    created_at_ms: i64,
    updated_at_ms: i64,
}

const fn feedback_event_key(event: FeedbackEvent) -> &'static str {
    match event {
        FeedbackEvent::Injected => "injected",
        FeedbackEvent::Used => "used",
        FeedbackEvent::Ignored => "ignored",
        FeedbackEvent::Error => "error",
    }
}

fn stored_memory_from_doc(document: &Doc) -> Result<StoredMemory> {
    let id = document
        .get_pk()
        .ok_or_else(|| anyhow!("zvec result is missing its primary key"))?
        .to_string();
    let tags_json = required_string(document, "tags")?;
    Ok(StoredMemory {
        id,
        title: required_string(document, "title")?,
        content: required_string(document, "content")?,
        kind: MemoryKind::parse(&required_string(document, "kind")?)?,
        importance: document.get_f32("importance")?.unwrap_or_default(),
        tags: serde_json::from_str(&tags_json).context("invalid tags stored in memory")?,
        source: required_string(document, "source")?,
        created_at_ms: document.get_i64("created_at")?.unwrap_or_default(),
        updated_at_ms: document.get_i64("updated_at")?.unwrap_or_default(),
    })
}

fn decorate_memory(stored: StoredMemory, metadata: MemoryMetadata, stale: bool) -> MemoryRecord {
    MemoryRecord {
        id: stored.id,
        title: stored.title,
        content: stored.content,
        kind: stored.kind,
        importance: stored.importance,
        tags: stored.tags,
        source: stored.source,
        created_at_ms: stored.created_at_ms,
        updated_at_ms: stored.updated_at_ms,
        scope: metadata.scope,
        origin: metadata.origin,
        expires_at_ms: metadata.expires_at_ms,
        pinned: metadata.pinned,
        locked: metadata.locked,
        lock_reason: metadata.lock_reason,
        stale,
        code_anchors: metadata.code_anchors,
        feedback: metadata.feedback,
        taxonomy: metadata.taxonomy,
        confidence: metadata.confidence,
        superseded_by: metadata.superseded_by,
        supersedes: metadata.supersedes,
        conflict_with: metadata.conflict_with,
        score: None,
        score_breakdown: None,
    }
}

fn required_string(document: &Doc, field: &str) -> Result<String> {
    document
        .get_string(field)?
        .ok_or_else(|| anyhow!("zvec result is missing field {field}"))
}

fn build_search_text(title: &str, kind: MemoryKind, tags: &[String], content: &str) -> String {
    format!(
        "{title}\nkind: {}\ntags: {}\n{content}",
        kind.as_str(),
        tags.join(", ")
    )
}

fn initial_expiry(
    normalized: &NormalizedStoreRequest,
    created_at_ms: i64,
    now_ms: i64,
) -> Option<i64> {
    normalized.expires_in_days.map_or_else(
        || {
            if normalized.scope == MemoryScope::Session {
                expiry_from_days(now_ms, Some(SESSION_DEFAULT_TTL_DAYS))
            } else {
                default_expiry(normalized.kind, created_at_ms)
            }
        },
        |days| expiry_from_days(now_ms, Some(days)),
    )
}

fn deterministic_memory_id(
    kind: MemoryKind,
    scope: MemoryScope,
    scope_key: Option<&str>,
    content: &str,
) -> String {
    let material = format!(
        "{}\0{}\0{}\0{}",
        kind.as_str(),
        scope.as_str(),
        scope_key.unwrap_or_default(),
        content
    );
    let hash = hash_hex(material.as_bytes());
    format!("mem_{}", &hash[..32])
}

fn legacy_memory_id(kind: MemoryKind, content: &str) -> String {
    let hash = hash_hex(format!("{}\0{content}", kind.as_str()).as_bytes());
    format!("mem_{}", &hash[..32])
}

fn management_visible(
    metadata: &MemoryMetadata,
    session_scope_key: Option<&str>,
    agent_scope_key: Option<&str>,
) -> bool {
    match metadata.scope {
        MemoryScope::Session => metadata.scope_key.as_deref() == session_scope_key,
        MemoryScope::Agent => metadata.scope_key.as_deref() == agent_scope_key,
        MemoryScope::Project | MemoryScope::Repository => true,
    }
}

fn now_ms() -> Result<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?;
    i64::try_from(duration.as_millis()).context("system timestamp exceeds i64")
}
