use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result, anyhow, bail, ensure};

use super::*;
use crate::capture::{CaptureSafety, SourceTrust};
use crate::graph::{
    DEFAULT_GRAPH_JOB_ATTEMPTS, DEFAULT_GRAPH_JOB_LEASE_MS, GRAPH_POLICY_VERSION,
    GraphCandidateConflict, GraphCandidateRejection, GraphEntity as StoredGraphEntity,
    GraphEntityInput, GraphEvidence, GraphEvidenceInput, GraphExtractionJob, GraphJobFinishOutcome,
    GraphJobState, GraphProvider, GraphRelation as StoredGraphRelation, GraphRelationInput,
    GraphScope, GraphScopeKind, GraphSource, MAX_GRAPH_DEPTH, MAX_GRAPH_ENTITIES,
    MAX_GRAPH_EVIDENCE, MAX_GRAPH_FANOUT, MAX_GRAPH_JOB_ATTEMPTS, MAX_GRAPH_JOB_LEASE_MS,
    MAX_GRAPH_PAGE, MAX_GRAPH_QUOTE_CHARS, MAX_GRAPH_RELATIONS, MAX_GRAPH_RESULTS,
    MAX_GRAPH_TEXT_BYTES, MAX_GRAPH_TOTAL_TEXT_BYTES, MAX_GRAPH_UNITS,
};
use crate::graph_proto::{
    GraphAcceptedEntity, GraphAcceptedRelation, GraphAuthorization,
    GraphCandidateConflict as ProtoGraphCandidateConflict, GraphCandidateEvidence,
    GraphDerivedScope, GraphEntity, GraphEntityCandidate, GraphEntitySearchResult,
    GraphEvidenceProvenance, GraphExportProvenance, GraphExportRequest, GraphExportResponse,
    GraphExtractCancelRequest, GraphExtractCancelResponse, GraphExtractClaimRequest,
    GraphExtractClaimResponse, GraphExtractEnqueueRequest, GraphExtractEnqueueResponse,
    GraphExtractFinishOutcome, GraphExtractFinishRequest, GraphExtractFinishResponse,
    GraphExtractJobStatusRequest, GraphExtractJobStatusResponse, GraphExtractPrepareRequest,
    GraphExtractPrepareResponse, GraphExtractRenewRequest, GraphExtractRenewResponse,
    GraphExtractionJobState, GraphExtractionUnit, GraphLastExtraction, GraphMemorySearchResult,
    GraphProviderIdentity, GraphRejectedCandidate as ProtoGraphCandidateRejection,
    GraphRejectedSource, GraphRelation, GraphRelationCandidate, GraphRelationSearchResult,
    GraphRunReceipt, GraphRunStatusRequest, GraphRunStatusResponse, GraphScopeFilter,
    GraphSearchRequest, GraphSearchResponse, GraphSourceBinding, GraphStatusRequest,
    GraphStatusResponse, GraphTimeFilter, GraphUpsertCandidatesRequest,
    GraphUpsertCandidatesResponse,
};

impl MemoryEngine {
    pub(crate) fn graph_extract_prepare(
        &self,
        request: &GraphExtractPrepareRequest,
    ) -> Result<GraphExtractPrepareResponse> {
        let authorization = graph_authorization(request.authorization.as_ref())?;
        let max_units = bounded_limit(request.max_units, MAX_GRAPH_UNITS, MAX_GRAPH_UNITS)?;
        let max_unit_text_bytes = bounded_limit(
            request.max_unit_text_bytes,
            MAX_GRAPH_TEXT_BYTES,
            MAX_GRAPH_TEXT_BYTES,
        )?;
        let max_total_text_bytes = bounded_limit(
            request.max_total_text_bytes,
            MAX_GRAPH_TOTAL_TEXT_BYTES,
            MAX_GRAPH_TOTAL_TEXT_BYTES,
        )?;
        ensure!(
            request.source_memory_ids.len() <= MAX_GRAPH_UNITS,
            "graph source count exceeds limit"
        );
        let mut units = Vec::new();
        let mut rejected_sources = Vec::new();
        let mut warnings = Vec::new();
        let mut seen = HashSet::new();
        let mut total_bytes = 0_usize;
        for id in &request.source_memory_ids {
            if !seen.insert(id.clone()) {
                rejected_sources.push(GraphRejectedSource {
                    source_memory_id: id.clone(),
                    code: "duplicate_source".to_string(),
                    message: "source memory ID was requested more than once".to_string(),
                });
                continue;
            }
            if units.len() >= max_units {
                rejected_sources.push(GraphRejectedSource {
                    source_memory_id: id.clone(),
                    code: "unit_limit".to_string(),
                    message: "source unit limit was reached".to_string(),
                });
                continue;
            }
            let source = match self.graph_source_snapshot(id, Some(authorization)) {
                Ok(source) => source,
                Err(error) => {
                    rejected_sources.push(GraphRejectedSource {
                        source_memory_id: id.clone(),
                        code: source_error_code(&error),
                        message: source_error_message(&error),
                    });
                    continue;
                }
            };
            let text = truncate_utf8(&source.text, max_unit_text_bytes);
            let text_bytes = text.len();
            if total_bytes.saturating_add(text_bytes) > max_total_text_bytes {
                rejected_sources.push(GraphRejectedSource {
                    source_memory_id: id.clone(),
                    code: "text_limit".to_string(),
                    message: "graph extraction text budget was reached".to_string(),
                });
                continue;
            }
            total_bytes = total_bytes.saturating_add(text_bytes);
            let remote_reason = graph_remote_ineligible_reason(&source);
            if remote_reason.is_some() {
                warnings.push(format!(
                    "{} is not eligible for remote graph extraction",
                    source.source_memory_id
                ));
            }
            let mut bounded_source = source;
            bounded_source.text = text;
            units.push(GraphExtractionUnit {
                source: Some(graph_source_binding(&bounded_source)),
                text: bounded_source.text,
                remote_ineligible_reason: remote_reason,
            });
        }
        Ok(GraphExtractPrepareResponse {
            requested_source_count: request.source_memory_ids.len() as u64,
            units,
            rejected_sources,
            warnings,
        })
    }

    pub(crate) fn graph_extract_enqueue(
        &mut self,
        request: &GraphExtractEnqueueRequest,
    ) -> Result<GraphExtractEnqueueResponse> {
        let authorization = graph_authorization(request.authorization.as_ref())?;
        let provider = request
            .provider
            .as_ref()
            .ok_or_else(|| anyhow!("graph provider identity is required"))?;
        let max_attempts = if request.max_attempts == 0 {
            DEFAULT_GRAPH_JOB_ATTEMPTS
        } else {
            request.max_attempts
        };
        ensure!(
            (1..=MAX_GRAPH_JOB_ATTEMPTS).contains(&max_attempts),
            "graph job attempt limit is invalid"
        );
        let max_unit_text_bytes = if request.max_unit_text_bytes == 0 {
            MAX_GRAPH_TEXT_BYTES as u32
        } else {
            request.max_unit_text_bytes
        };
        let max_total_text_bytes = if request.max_total_text_bytes == 0 {
            MAX_GRAPH_TOTAL_TEXT_BYTES as u32
        } else {
            request.max_total_text_bytes
        };
        ensure!(
            (1..=MAX_GRAPH_TEXT_BYTES as u32).contains(&max_unit_text_bytes)
                && (max_unit_text_bytes..=MAX_GRAPH_TOTAL_TEXT_BYTES as u32)
                    .contains(&max_total_text_bytes),
            "graph job text limits are invalid"
        );
        ensure!(
            request.source_memory_ids.len() <= MAX_GRAPH_UNITS,
            "graph job source count exceeds limit"
        );
        let mut seen = HashSet::new();
        let mut sources = Vec::new();
        let mut rejected_sources = Vec::new();
        let mut total_bytes = 0_usize;
        for id in &request.source_memory_ids {
            if !seen.insert(id.clone()) {
                rejected_sources.push(GraphRejectedSource {
                    source_memory_id: id.clone(),
                    code: "duplicate_source".to_string(),
                    message: "source memory ID was requested more than once".to_string(),
                });
                continue;
            }
            match self.graph_source_snapshot(id, Some(authorization)) {
                Ok(source) if !source.remote_eligible => {
                    rejected_sources.push(GraphRejectedSource {
                        source_memory_id: id.clone(),
                        code: "remote_ineligible".to_string(),
                        message: "source is not eligible for remote graph extraction".to_string(),
                    })
                }
                Ok(source) => {
                    let text = truncate_utf8(&source.text, max_unit_text_bytes as usize);
                    if total_bytes.saturating_add(text.len()) > max_total_text_bytes as usize {
                        rejected_sources.push(GraphRejectedSource {
                            source_memory_id: id.clone(),
                            code: "text_limit".to_string(),
                            message: "graph job total text limit was reached".to_string(),
                        });
                    } else {
                        total_bytes = total_bytes.saturating_add(text.len());
                        sources.push(source);
                    }
                }
                Err(error) => rejected_sources.push(GraphRejectedSource {
                    source_memory_id: id.clone(),
                    code: source_error_code(&error),
                    message: source_error_message(&error),
                }),
            }
        }
        ensure!(
            !sources.is_empty(),
            "no source is eligible for graph extraction"
        );
        let (job, existing) = self.graph.enqueue_job(
            &request.job_id,
            &sources,
            &graph_provider(provider),
            max_attempts,
            max_unit_text_bytes,
            max_total_text_bytes,
            now_ms_u64()?,
        )?;
        Ok(GraphExtractEnqueueResponse {
            job: Some(proto_graph_job(&job)),
            existing,
            rejected_sources,
            warnings: Vec::new(),
        })
    }

    pub(crate) fn graph_extract_claim(
        &mut self,
        request: &GraphExtractClaimRequest,
    ) -> Result<GraphExtractClaimResponse> {
        let authorization = graph_authorization(request.authorization.as_ref())?;
        let lease_ttl_ms = if request.lease_ttl_ms == 0 {
            DEFAULT_GRAPH_JOB_LEASE_MS
        } else {
            request.lease_ttl_ms
        };
        ensure!(
            lease_ttl_ms <= MAX_GRAPH_JOB_LEASE_MS,
            "graph job lease duration is invalid"
        );
        let claimed = self.graph.claim_job(
            request.job_id.as_deref(),
            &request.claim_request_id,
            &request.worker_id,
            lease_ttl_ms,
            now_ms_u64()?,
            |job| graph_job_visible(job, authorization),
        )?;
        let Some(claimed) = claimed else {
            return Ok(GraphExtractClaimResponse {
                found: false,
                job: None,
                lease_token: String::new(),
                units: Vec::new(),
                rejected_sources: Vec::new(),
                warnings: Vec::new(),
            });
        };
        let sources = match self.validate_graph_job_sources(&claimed.job, authorization) {
            Ok(sources) => sources,
            Err(error) => {
                let failed = self.graph.mark_job_source_changed(
                    &claimed.job.job_id,
                    &error.to_string(),
                    now_ms_u64()?,
                )?;
                return Ok(GraphExtractClaimResponse {
                    found: true,
                    job: Some(proto_graph_job(&failed)),
                    lease_token: String::new(),
                    units: Vec::new(),
                    rejected_sources: Vec::new(),
                    warnings: vec!["graph job source changed before claim".to_string()],
                });
            }
        };
        let units = graph_job_units(&claimed.job, &sources)?;
        Ok(GraphExtractClaimResponse {
            found: true,
            job: Some(proto_graph_job(&claimed.job)),
            lease_token: claimed.lease_token,
            units,
            rejected_sources: Vec::new(),
            warnings: Vec::new(),
        })
    }

    pub(crate) fn graph_extract_renew(
        &mut self,
        request: &GraphExtractRenewRequest,
    ) -> Result<GraphExtractRenewResponse> {
        let authorization = graph_authorization(request.authorization.as_ref())?;
        self.graph
            .job(&request.job_id)
            .filter(|job| graph_job_visible(job, authorization))
            .ok_or_else(|| anyhow!("graph job not found: {}", request.job_id))?;
        let lease_ttl_ms = if request.lease_ttl_ms == 0 {
            DEFAULT_GRAPH_JOB_LEASE_MS
        } else {
            request.lease_ttl_ms
        };
        let renewed = self.graph.renew_job(
            &request.job_id,
            &request.lease_token,
            lease_ttl_ms,
            now_ms_u64()?,
        )?;
        Ok(GraphExtractRenewResponse {
            lease_expires_at_ms: renewed.lease_expires_at_ms.unwrap_or_default(),
            cancel_requested: renewed.cancel_requested,
            job: Some(proto_graph_job(&renewed)),
        })
    }

    pub(crate) fn graph_extract_finish(
        &mut self,
        request: &GraphExtractFinishRequest,
    ) -> Result<GraphExtractFinishResponse> {
        let authorization = graph_authorization(request.authorization.as_ref())?;
        let current = self
            .graph
            .job(&request.job_id)
            .filter(|job| graph_job_visible(job, authorization))
            .cloned()
            .ok_or_else(|| anyhow!("graph job not found: {}", request.job_id))?;
        let outcome = GraphExtractFinishOutcome::try_from(request.outcome)
            .map_err(|_| anyhow!("graph job finish outcome is unknown"))?;
        let outcome = match outcome {
            GraphExtractFinishOutcome::Completed => GraphJobFinishOutcome::Completed,
            GraphExtractFinishOutcome::RetryableFailure => GraphJobFinishOutcome::RetryableFailure,
            GraphExtractFinishOutcome::PermanentFailure => GraphJobFinishOutcome::PermanentFailure,
            GraphExtractFinishOutcome::Unspecified => {
                bail!("graph job finish outcome is required")
            }
        };
        let (entities, relations) = if outcome == GraphJobFinishOutcome::Completed {
            (
                request
                    .entities
                    .iter()
                    .map(graph_entity_input)
                    .collect::<Result<Vec<_>>>()?,
                request
                    .relations
                    .iter()
                    .map(graph_relation_input)
                    .collect::<Result<Vec<_>>>()?,
            )
        } else {
            ensure!(
                request.entities.is_empty() && request.relations.is_empty(),
                "failed graph job cannot include candidates"
            );
            (Vec::new(), Vec::new())
        };
        let sources = if outcome == GraphJobFinishOutcome::Completed {
            match self.validate_graph_job_sources(&current, authorization) {
                Ok(sources) => sources,
                Err(error) => {
                    self.graph
                        .mark_job_source_changed(&current.job_id, &error.to_string(), now_ms_u64()?)
                        .context("cannot persist graph job source-change fence")?;
                    return Err(error).context("graph job source changed before commit");
                }
            }
        } else {
            Vec::new()
        };
        let finished = self.graph.finish_job(
            &current.job_id,
            &request.lease_token,
            &request.extraction_run_id,
            outcome,
            &sources,
            &entities,
            &relations,
            &request.error_code,
            &request.error_message,
            now_ms_u64()?,
        )?;
        let upsert = finished
            .upsert
            .as_ref()
            .map(|outcome| proto_upsert_response(outcome, &entities, &relations));
        Ok(GraphExtractFinishResponse {
            job: Some(proto_graph_job(&finished.job)),
            upsert,
            warnings: finished.warnings,
        })
    }

    pub(crate) fn graph_extract_job_status(
        &mut self,
        request: &GraphExtractJobStatusRequest,
    ) -> Result<GraphExtractJobStatusResponse> {
        let authorization = graph_authorization(request.authorization.as_ref())?;
        self.graph.recover_job_leases(now_ms_u64()?)?;
        let Some(job) = self.graph.job(&request.job_id) else {
            return Ok(GraphExtractJobStatusResponse {
                found: false,
                job: None,
            });
        };
        if !graph_job_visible(job, authorization) {
            return Ok(GraphExtractJobStatusResponse {
                found: false,
                job: None,
            });
        }
        Ok(GraphExtractJobStatusResponse {
            found: true,
            job: Some(proto_graph_job(job)),
        })
    }

    pub(crate) fn graph_extract_cancel(
        &mut self,
        request: &GraphExtractCancelRequest,
    ) -> Result<GraphExtractCancelResponse> {
        let authorization = graph_authorization(request.authorization.as_ref())?;
        self.graph
            .job(&request.job_id)
            .filter(|job| graph_job_visible(job, authorization))
            .ok_or_else(|| anyhow!("graph job not found: {}", request.job_id))?;
        let (job, outcome) =
            self.graph
                .cancel_job(&request.job_id, &request.reason, now_ms_u64()?)?;
        Ok(GraphExtractCancelResponse {
            job: Some(proto_graph_job(&job)),
            outcome,
        })
    }

    pub(crate) fn graph_upsert_candidates(
        &mut self,
        request: &GraphUpsertCandidatesRequest,
    ) -> Result<GraphUpsertCandidatesResponse> {
        let authorization = graph_authorization(request.authorization.as_ref())?;
        let provider = request
            .provider
            .as_ref()
            .ok_or_else(|| anyhow!("graph provider identity is required"))?;
        ensure!(
            request.entities.len() <= MAX_GRAPH_ENTITIES,
            "graph entity count exceeds limit"
        );
        ensure!(
            request.relations.len() <= MAX_GRAPH_RELATIONS,
            "graph relation count exceeds limit"
        );
        let entities = request
            .entities
            .iter()
            .map(graph_entity_input)
            .collect::<Result<Vec<_>>>()?;
        let relations = request
            .relations
            .iter()
            .map(graph_relation_input)
            .collect::<Result<Vec<_>>>()?;
        let sources = if let Some(run) = self.graph.run(&request.extraction_run_id) {
            ensure!(
                run.scopes
                    .iter()
                    .all(|scope| graph_scope_visible(scope, authorization)),
                "graph extraction run is not visible to the current session or agent"
            );
            request
                .sources
                .iter()
                .map(graph_source_from_binding)
                .collect::<Result<Vec<_>>>()?
        } else {
            self.validate_graph_bindings(&request.sources, Some(authorization))?
        };
        ensure!(
            sources.len() <= MAX_GRAPH_UNITS,
            "graph source count exceeds limit"
        );
        let outcome = self.graph.upsert(
            &request.extraction_run_id,
            &sources,
            &graph_provider(provider),
            &entities,
            &relations,
            now_ms_u64()?,
        )?;
        let accepted_entities = outcome
            .entities
            .iter()
            .map(|(index, entity)| GraphAcceptedEntity {
                candidate_index: *index as u32,
                entity_id: entity.entity_id.clone(),
                canonical_name: entity.canonical_name.clone(),
                entity_type: entity.entity_type.clone(),
                derived_scope: Some(graph_derived_scope(&entity.scope)),
                evidence: entities
                    .get(*index)
                    .map(|candidate| {
                        candidate
                            .evidence
                            .iter()
                            .map(proto_candidate_evidence)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect();
        let accepted_relations = outcome
            .relations
            .iter()
            .map(|(index, relation)| GraphAcceptedRelation {
                candidate_index: *index as u32,
                relation_id: relation.relation_id.clone(),
                subject_entity_id: relation.subject_entity_id.clone(),
                predicate: relation.predicate.clone(),
                object_entity_id: relation.object_entity_id.clone(),
                relation_type: relation.relation_type.clone(),
                derived_scope: Some(graph_derived_scope(&relation.scope)),
                evidence: relations
                    .get(*index)
                    .map(|candidate| {
                        candidate
                            .evidence
                            .iter()
                            .map(proto_candidate_evidence)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect();
        Ok(GraphUpsertCandidatesResponse {
            receipt: Some(proto_run_receipt(&outcome.run)),
            accepted_entities,
            accepted_relations,
            rejected_candidates: outcome.rejected.iter().map(proto_rejection).collect(),
            conflicts: outcome.conflicts.iter().map(proto_conflict).collect(),
            warnings: outcome.warnings,
        })
    }

    pub(crate) fn graph_run_status(
        &self,
        request: &GraphRunStatusRequest,
    ) -> Result<GraphRunStatusResponse> {
        let authorization = graph_authorization(request.authorization.as_ref())?;
        let Some(run) = self.graph.run(&request.extraction_run_id) else {
            return Ok(GraphRunStatusResponse {
                found: false,
                receipt: None,
            });
        };
        if !run
            .scopes
            .iter()
            .all(|scope| graph_scope_visible(scope, authorization))
        {
            return Ok(GraphRunStatusResponse {
                found: false,
                receipt: None,
            });
        }
        Ok(GraphRunStatusResponse {
            found: true,
            receipt: Some(proto_run_receipt(run)),
        })
    }

    pub(crate) fn graph_search(&self, request: &GraphSearchRequest) -> Result<GraphSearchResponse> {
        let authorization = graph_authorization(request.authorization.as_ref())?;
        let max_depth = bounded_limit(request.max_depth, MAX_GRAPH_DEPTH, MAX_GRAPH_DEPTH)?;
        let max_fanout = bounded_limit(request.max_fanout, MAX_GRAPH_FANOUT, MAX_GRAPH_FANOUT)?;
        let max_results = bounded_limit(request.max_results, MAX_GRAPH_RESULTS, MAX_GRAPH_RESULTS)?;
        let max_evidence = bounded_limit(
            request.max_evidence_per_fact,
            MAX_GRAPH_EVIDENCE,
            MAX_GRAPH_EVIDENCE,
        )?;
        validate_graph_time_filter(request.time.as_ref())?;
        let current_ms = now_ms_u64()?;
        let (eligible_entity_ids, eligible_relation_ids) = self.graph_search_eligible_ids(
            authorization,
            request.scope.as_ref(),
            request.time.as_ref(),
            current_ms,
        );
        let stored = self.graph.search(
            &request.query,
            max_depth,
            max_fanout,
            max_results,
            &eligible_entity_ids,
            &eligible_relation_ids,
        )?;
        let mut memories = Vec::new();
        let mut entities = Vec::new();
        let mut relations = Vec::new();
        let mut eligible_sources = HashSet::new();
        let mut seen_relations = HashSet::new();
        let mut seen_memories = HashSet::new();
        for item in stored {
            let mut memory_provenance = Vec::new();
            for entity in &item.entities {
                if !graph_scope_filter_matches(&entity.scope, request.scope.as_ref()) {
                    continue;
                }
                let provenance = self.entity_provenance(entity, authorization, max_evidence)?;
                if provenance.is_empty() {
                    continue;
                }
                if memory_provenance.is_empty() {
                    memory_provenance.clone_from(&provenance);
                }
                eligible_sources.extend(
                    provenance
                        .iter()
                        .map(|entry| entry.source_memory_id.clone()),
                );
                entities.push(GraphEntitySearchResult {
                    entity: Some(proto_entity(entity)),
                    score: item.score,
                    provenance,
                    score_trace: vec![crate::graph_proto::GraphScoreComponent {
                        name: "lexical_bfs".to_string(),
                        value: item.score,
                    }],
                });
            }
            for relation in &item.relations {
                if !seen_relations.insert(relation.relation_id.clone())
                    || !graph_scope_filter_matches(&relation.scope, request.scope.as_ref())
                    || !graph_time_filter_matches(relation, request.time.as_ref(), current_ms)
                    || !self.graph_entity_current(&relation.subject_entity_id, authorization)
                    || !self.graph_entity_current(&relation.object_entity_id, authorization)
                {
                    continue;
                }
                let provenance =
                    self.active_provenance(&relation.evidence, authorization, max_evidence)?;
                if provenance.is_empty() {
                    continue;
                }
                eligible_sources.extend(
                    provenance
                        .iter()
                        .map(|entry| entry.source_memory_id.clone()),
                );
                relations.push(GraphRelationSearchResult {
                    relation: Some(proto_relation(relation)),
                    score: item.score,
                    provenance,
                    score_trace: vec![crate::graph_proto::GraphScoreComponent {
                        name: "lexical_bfs".to_string(),
                        value: item.score,
                    }],
                });
            }
            if memory_provenance.is_empty() {
                memory_provenance =
                    self.active_provenance(&item.evidence, authorization, max_evidence)?;
            }
            let memory_id = memory_provenance
                .first()
                .map(|entry| entry.source_memory_id.clone())
                .unwrap_or(item.memory_id);
            if !memory_provenance.is_empty() && seen_memories.insert(memory_id.clone()) {
                eligible_sources.extend(
                    memory_provenance
                        .iter()
                        .map(|entry| entry.source_memory_id.clone()),
                );
                memories.push(GraphMemorySearchResult {
                    source_memory_id: memory_id,
                    score: item.score,
                    provenance: memory_provenance,
                    score_trace: vec![crate::graph_proto::GraphScoreComponent {
                        name: "lexical_bfs".to_string(),
                        value: item.score,
                    }],
                });
            }
        }
        let truncated = stored_len_exceeded(&memories, &entities, &relations, max_results);
        memories.truncate(max_results);
        entities.truncate(max_results);
        relations.truncate(max_results);
        Ok(GraphSearchResponse {
            memories,
            entities,
            relations,
            eligible_source_count: eligible_sources.len() as u64,
            truncated,
        })
    }

    pub(crate) fn graph_ranked_memory_ids(
        &self,
        query: &str,
        request: &SearchRequest,
        limit: usize,
    ) -> Result<Vec<String>> {
        let (Some(session_scope_key), Some(agent_scope_key)) = (
            request.session_scope_key.as_deref(),
            request.agent_scope_key.as_deref(),
        ) else {
            return Ok(Vec::new());
        };
        let scope = (request.scopes.len() == 1).then(|| {
            let memory_scope = request.scopes[0].as_str().to_string();
            let verified_scope_key = match request.scopes[0] {
                MemoryScope::Session => session_scope_key.to_string(),
                MemoryScope::Agent => agent_scope_key.to_string(),
                MemoryScope::Project | MemoryScope::Repository => String::new(),
            };
            GraphScopeFilter {
                memory_scope: Some(memory_scope),
                verified_scope_key: Some(verified_scope_key),
            }
        });
        let response = self.graph_search(&GraphSearchRequest {
            authorization: Some(GraphAuthorization {
                session_scope_key: session_scope_key.to_string(),
                agent_scope_key: agent_scope_key.to_string(),
            }),
            query: query.to_string(),
            scope,
            time: None,
            max_depth: MAX_GRAPH_DEPTH as u32,
            max_fanout: MAX_GRAPH_FANOUT as u32,
            max_results: limit as u32,
            max_evidence_per_fact: MAX_GRAPH_EVIDENCE as u32,
        })?;
        Ok(response
            .memories
            .into_iter()
            .map(|memory| memory.source_memory_id)
            .take(limit)
            .collect())
    }

    pub(crate) fn graph_status(
        &mut self,
        request: &GraphStatusRequest,
    ) -> Result<GraphStatusResponse> {
        let authorization = graph_authorization(request.authorization.as_ref())?;
        let current_ms = now_ms_u64()?;
        self.graph.recover_job_leases(current_ms)?;
        let mut entity_ids = HashSet::new();
        for mention in &self.graph.state().mentions {
            if graph_scope_filter_matches(&mention.evidence.scope, request.scope.as_ref())
                && self
                    .graph_evidence_current(&mention.evidence, authorization)
                    .is_ok()
            {
                entity_ids.insert(mention.entity_id.clone());
            }
        }
        let mut relation_count = 0_u64;
        let mut last_extraction = None;
        for relation in self.graph.state().relations.values() {
            if !graph_scope_filter_matches(&relation.scope, request.scope.as_ref())
                || !graph_relation_valid_at(relation, current_ms)
                || !self.graph_entity_current(&relation.subject_entity_id, authorization)
                || !self.graph_entity_current(&relation.object_entity_id, authorization)
                || !relation
                    .evidence
                    .iter()
                    .any(|evidence| self.graph_evidence_current(evidence, authorization).is_ok())
            {
                continue;
            }
            relation_count = relation_count.saturating_add(1);
        }
        for run in self.graph.state().runs.values() {
            if run
                .scopes
                .iter()
                .all(|scope| graph_scope_visible(scope, authorization))
                && last_extraction
                    .as_ref()
                    .is_none_or(|current: &GraphLastExtraction| {
                        current.completed_at_ms < run.committed_at_ms
                    })
            {
                last_extraction = Some(GraphLastExtraction {
                    extraction_run_id: run.extraction_run_id.clone(),
                    completed_at_ms: run.committed_at_ms,
                    source_count: run.source_count,
                });
            }
        }
        let pending_job_count = self
            .graph
            .state()
            .jobs
            .values()
            .filter(|job| {
                !job.state.is_terminal()
                    && graph_job_visible(job, authorization)
                    && job.sources.iter().all(|source| {
                        graph_scope_filter_matches(&source.scope, request.scope.as_ref())
                    })
            })
            .count() as u64;
        Ok(GraphStatusResponse {
            schema_version: self.graph.state().schema_version.to_string(),
            entity_count: entity_ids.len() as u64,
            relation_count,
            pending_job_count,
            last_extraction,
        })
    }

    pub(crate) fn graph_export(&self, request: &GraphExportRequest) -> Result<GraphExportResponse> {
        let authorization = graph_authorization(request.authorization.as_ref())?;
        let page_limit = bounded_limit(request.page_limit, MAX_GRAPH_PAGE, MAX_GRAPH_PAGE)?;
        let current_ms = now_ms_u64()?;
        let cursor = request
            .cursor
            .as_deref()
            .unwrap_or("0")
            .parse::<usize>()
            .context("graph export cursor is invalid")?;
        let mut items = Vec::<(String, String)>::new();
        for entity in self.graph.state().entities.values() {
            if graph_scope_filter_matches(&entity.scope, request.scope.as_ref())
                && self.graph.state().mentions.iter().any(|mention| {
                    mention.entity_id == entity.entity_id
                        && self
                            .graph_evidence_current(&mention.evidence, authorization)
                            .is_ok()
                })
            {
                items.push(("entity".to_string(), entity.entity_id.clone()));
            }
        }
        for relation in self.graph.state().relations.values() {
            if graph_scope_filter_matches(&relation.scope, request.scope.as_ref())
                && graph_relation_valid_at(relation, current_ms)
                && self.graph_entity_current(&relation.subject_entity_id, authorization)
                && self.graph_entity_current(&relation.object_entity_id, authorization)
                && relation
                    .evidence
                    .iter()
                    .any(|evidence| self.graph_evidence_current(evidence, authorization).is_ok())
            {
                items.push(("relation".to_string(), relation.relation_id.clone()));
            }
        }
        ensure!(
            cursor <= items.len(),
            "graph export cursor is outside the result set"
        );
        let page = &items[cursor..items.len().min(cursor.saturating_add(page_limit))];
        let mut entities = Vec::new();
        let mut relations = Vec::new();
        let mut provenance = Vec::new();
        for (kind, id) in page {
            if kind == "entity" {
                let entity = self
                    .graph
                    .entity(id)
                    .ok_or_else(|| anyhow!("graph export entity disappeared"))?;
                entities.push(proto_entity(entity));
                provenance.push(GraphExportProvenance {
                    fact_kind: kind.clone(),
                    fact_id: id.clone(),
                    sources: self.entity_provenance(entity, authorization, MAX_GRAPH_EVIDENCE)?,
                });
            } else {
                let relation = self
                    .graph
                    .relation(id)
                    .ok_or_else(|| anyhow!("graph export relation disappeared"))?;
                relations.push(proto_relation(relation));
                provenance.push(GraphExportProvenance {
                    fact_kind: kind.clone(),
                    fact_id: id.clone(),
                    sources: self.active_provenance(
                        &relation.evidence,
                        authorization,
                        MAX_GRAPH_EVIDENCE,
                    )?,
                });
            }
        }
        let next_cursor =
            (cursor + page.len() < items.len()).then(|| (cursor + page.len()).to_string());
        let complete = next_cursor.is_none();
        Ok(GraphExportResponse {
            schema_version: self.graph.state().schema_version.to_string(),
            entities,
            relations,
            provenance,
            next_cursor,
            complete,
        })
    }

    pub(crate) fn ensure_graph_source_visibility(
        &self,
        ids: &[String],
        session: Option<&str>,
        agent: Option<&str>,
    ) -> Result<()> {
        let source_ids = ids.iter().cloned().collect::<HashSet<_>>();
        for scope in self.graph.scopes_for_source_ids(&source_ids) {
            ensure!(
                match scope.kind {
                    GraphScopeKind::Session => scope.scope_key.as_deref() == session,
                    GraphScopeKind::Agent => scope.scope_key.as_deref() == agent,
                    GraphScopeKind::Project | GraphScopeKind::Repository => true,
                },
                "graph source is not visible to the current session or agent"
            );
        }
        Ok(())
    }

    pub(crate) fn invalidate_graph_sources_for_upserts(
        &mut self,
        pending: &[PendingUpsert],
    ) -> Result<()> {
        let mut source_ids = HashSet::new();
        for item in pending {
            source_ids.extend(item.predecessor_ids.iter().cloned());
            let old = self.fetch_documents(std::slice::from_ref(&item.document.id))?;
            if let Some(document) = old.first() {
                let old_metadata = self.state.metadata(&item.document.id).ok();
                let old_revision = old_metadata
                    .as_ref()
                    .map(|metadata| graph_revision(document, metadata));
                let new_revision = graph_pending_revision(&item.document, &item.metadata);
                if old_revision.as_deref() != Some(new_revision.as_str()) {
                    source_ids.insert(item.document.id.clone());
                }
            }
        }
        self.erase_graph_sources(&source_ids, "upsert")
    }

    pub(crate) fn erase_graph_sources(
        &mut self,
        ids: &HashSet<String>,
        reason: &str,
    ) -> Result<()> {
        self.graph.erase_sources(
            ids,
            &format!(
                "{reason}:{}",
                self.graph.state().generation.saturating_add(1)
            ),
        )
    }

    fn graph_source_snapshot(
        &self,
        id: &str,
        authorization: Option<&GraphAuthorization>,
    ) -> Result<GraphSource> {
        validate_ids(std::slice::from_ref(&id.to_string()))?;
        ensure!(
            !self.state.pending_deletes.contains(id),
            "memory is pending deletion"
        );
        ensure!(
            !self.state.pending_upserts.contains_key(id),
            "memory has a pending update"
        );
        let document = self
            .fetch_documents(std::slice::from_ref(&id.to_string()))?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("memory not found: {id}"))?;
        let stored = stored_memory_from_doc(&document)?;
        let metadata = self.state.metadata(id)?;
        if let Some(authorization) = authorization {
            ensure!(
                management_visible(
                    &metadata,
                    Some(&authorization.session_scope_key),
                    Some(&authorization.agent_scope_key)
                ),
                "memory is not visible to the current session or agent"
            );
        }
        let now = now_ms()?;
        ensure!(!is_expired(&metadata, now), "memory is expired");
        ensure!(
            !metadata.is_superseded(),
            "memory is superseded historical memory"
        );
        ensure!(
            !anchors_stale(&self.config, &metadata.code_anchors),
            "memory has stale code anchors"
        );
        let content_hash = required_string(&document, "content_hash")?;
        let scope = graph_scope(&self.config, &metadata);
        let remote_eligible = graph_remote_ineligible_reason_values(&stored, &metadata).is_none();
        Ok(GraphSource {
            source_memory_id: stored.id,
            source_unit_id: graph_source_unit_id(
                id,
                &content_hash,
                &scope,
                &metadata.origin,
                &stored.source,
            ),
            content_hash: content_hash.clone(),
            extraction_revision: graph_revision(&document, &metadata),
            scope,
            origin: memory_origin_name(metadata.origin).to_string(),
            policy_revision: GRAPH_POLICY_VERSION.to_string(),
            remote_eligible,
            text: stored.content,
        })
    }

    fn validate_graph_bindings(
        &self,
        bindings: &[GraphSourceBinding],
        authorization: Option<&GraphAuthorization>,
    ) -> Result<Vec<GraphSource>> {
        ensure!(
            !bindings.is_empty(),
            "graph upsert needs at least one source binding"
        );
        let mut sources = Vec::with_capacity(bindings.len());
        let mut seen = HashSet::new();
        for binding in bindings {
            ensure!(
                seen.insert(binding.source_memory_id.clone()),
                "duplicate graph source binding"
            );
            let source = self.graph_source_snapshot(&binding.source_memory_id, authorization)?;
            ensure!(
                binding.source_unit_id == source.source_unit_id,
                "graph source unit is stale"
            );
            ensure!(
                binding.content_hash == source.content_hash,
                "graph source content hash is stale"
            );
            ensure!(
                binding.extraction_revision == source.extraction_revision,
                "graph extraction revision is stale"
            );
            ensure!(
                binding.policy_revision == source.policy_revision,
                "graph policy revision is stale"
            );
            ensure!(
                binding.remote_eligible == source.remote_eligible,
                "graph remote eligibility changed"
            );
            ensure!(
                binding
                    .derived_scope
                    .as_ref()
                    .is_some_and(|scope| scope.project_id == source.scope.project_id
                        && scope.memory_scope == source.scope.kind.as_str()
                        && scope.verified_scope_key
                            == source.scope.scope_key.clone().unwrap_or_default()),
                "graph derived scope is not source-derived"
            );
            sources.push(source);
        }
        Ok(sources)
    }

    fn validate_graph_job_sources(
        &self,
        job: &GraphExtractionJob,
        authorization: &GraphAuthorization,
    ) -> Result<Vec<GraphSource>> {
        let bindings = job
            .sources
            .iter()
            .map(graph_job_source_binding)
            .collect::<Vec<_>>();
        self.validate_graph_bindings(&bindings, Some(authorization))
    }

    fn graph_evidence_current(
        &self,
        evidence: &GraphEvidence,
        authorization: &GraphAuthorization,
    ) -> Result<GraphSource> {
        let source = self.graph_source_snapshot(&evidence.source_memory_id, Some(authorization))?;
        ensure!(
            graph_evidence_matches_source(evidence, &source),
            "graph evidence is stale"
        );
        Ok(source)
    }

    fn active_provenance(
        &self,
        evidence: &[GraphEvidence],
        authorization: &GraphAuthorization,
        limit: usize,
    ) -> Result<Vec<GraphEvidenceProvenance>> {
        let active = select_active_evidence(evidence, limit, |item| {
            self.graph_evidence_current(item, authorization).is_ok()
        });
        let mut grouped = BTreeMap::<String, (GraphEvidence, Vec<GraphCandidateEvidence>)>::new();
        for item in active {
            let (_, source_evidence) = grouped
                .entry(item.source_memory_id.clone())
                .or_insert_with(|| (item.clone(), Vec::new()));
            source_evidence.push(proto_candidate_evidence_from_graph(item));
        }
        Ok(grouped
            .into_iter()
            .map(|(source_id, (item, evidence))| GraphEvidenceProvenance {
                source_memory_id: source_id,
                source_unit_id: item.source_unit_id,
                content_hash: item.content_hash,
                extraction_revision: item.extraction_revision,
                derived_scope: Some(graph_derived_scope(&item.scope)),
                evidence,
            })
            .collect())
    }

    fn graph_search_eligible_ids(
        &self,
        authorization: &GraphAuthorization,
        scope_filter: Option<&GraphScopeFilter>,
        time_filter: Option<&GraphTimeFilter>,
        current_ms: u64,
    ) -> (HashSet<String>, HashSet<String>) {
        let mut source_cache = HashMap::<String, Option<GraphSource>>::new();
        let mut entity_ids = HashSet::new();
        for mention in &self.graph.state().mentions {
            let Some(entity) = self.graph.entity(&mention.entity_id) else {
                continue;
            };
            if mention.evidence.scope == entity.scope
                && graph_scope_filter_matches(&entity.scope, scope_filter)
                && self.graph_evidence_current_cached(
                    &mention.evidence,
                    authorization,
                    &mut source_cache,
                )
            {
                entity_ids.insert(mention.entity_id.clone());
            }
        }

        let relation_ids = self
            .graph
            .state()
            .relations
            .iter()
            .filter(|(_, relation)| {
                let Some(subject) = self.graph.entity(&relation.subject_entity_id) else {
                    return false;
                };
                let Some(object) = self.graph.entity(&relation.object_entity_id) else {
                    return false;
                };
                graph_scope_filter_matches(&relation.scope, scope_filter)
                    && subject.scope == relation.scope
                    && object.scope == relation.scope
                    && graph_time_filter_matches(relation, time_filter, current_ms)
                    && entity_ids.contains(&relation.subject_entity_id)
                    && entity_ids.contains(&relation.object_entity_id)
                    && relation.evidence.iter().any(|evidence| {
                        self.graph_evidence_current_cached(
                            evidence,
                            authorization,
                            &mut source_cache,
                        )
                    })
            })
            .map(|(id, _)| id.clone())
            .collect::<HashSet<_>>();
        (entity_ids, relation_ids)
    }

    fn graph_evidence_current_cached(
        &self,
        evidence: &GraphEvidence,
        authorization: &GraphAuthorization,
        cache: &mut HashMap<String, Option<GraphSource>>,
    ) -> bool {
        if !cache.contains_key(&evidence.source_memory_id) {
            cache.insert(
                evidence.source_memory_id.clone(),
                self.graph_source_snapshot(&evidence.source_memory_id, Some(authorization))
                    .ok(),
            );
        }
        cache
            .get(&evidence.source_memory_id)
            .and_then(Option::as_ref)
            .is_some_and(|source| graph_evidence_matches_source(evidence, source))
    }

    fn graph_entity_current(&self, entity_id: &str, authorization: &GraphAuthorization) -> bool {
        let Some(entity) = self.graph.entity(entity_id) else {
            return false;
        };
        self.graph.state().mentions.iter().any(|mention| {
            mention.entity_id == entity_id
                && mention.evidence.scope == entity.scope
                && self
                    .graph_evidence_current(&mention.evidence, authorization)
                    .is_ok()
        })
    }

    fn entity_provenance(
        &self,
        entity: &StoredGraphEntity,
        authorization: &GraphAuthorization,
        limit: usize,
    ) -> Result<Vec<GraphEvidenceProvenance>> {
        let evidence = self
            .graph
            .state()
            .mentions
            .iter()
            .filter(|mention| mention.entity_id == entity.entity_id)
            .map(|mention| mention.evidence.clone())
            .collect::<Vec<_>>();
        self.active_provenance(&evidence, authorization, limit)
    }
}

fn select_active_evidence(
    evidence: &[GraphEvidence],
    limit: usize,
    mut is_current: impl FnMut(&GraphEvidence) -> bool,
) -> Vec<&GraphEvidence> {
    evidence
        .iter()
        .filter(|item| is_current(item))
        .take(limit)
        .collect()
}

fn graph_evidence_matches_source(evidence: &GraphEvidence, source: &GraphSource) -> bool {
    source.source_unit_id == evidence.source_unit_id
        && source.content_hash == evidence.content_hash
        && source.extraction_revision == evidence.extraction_revision
        && source.scope == evidence.scope
}

fn graph_authorization(value: Option<&GraphAuthorization>) -> Result<&GraphAuthorization> {
    let value = value.ok_or_else(|| anyhow!("graph authorization is required"))?;
    validate_graph_auth_key(&value.session_scope_key, "session_scope_key")?;
    validate_graph_auth_key(&value.agent_scope_key, "agent_scope_key")?;
    Ok(value)
}

fn validate_graph_auth_key(value: &str, name: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.chars().count() <= 240 && !value.contains('\0'),
        "graph {name} is invalid"
    );
    Ok(())
}

fn bounded_limit(value: u32, maximum: usize, default: usize) -> Result<usize> {
    let value = if value == 0 { default } else { value as usize };
    ensure!(
        (1..=maximum).contains(&value),
        "graph limit is outside the supported range"
    );
    Ok(value)
}

fn graph_scope(config: &MemoryConfig, metadata: &MemoryMetadata) -> GraphScope {
    GraphScope {
        project_id: config.project_id().to_string(),
        kind: match metadata.scope {
            MemoryScope::Project => GraphScopeKind::Project,
            MemoryScope::Repository => GraphScopeKind::Repository,
            MemoryScope::Agent => GraphScopeKind::Agent,
            MemoryScope::Session => GraphScopeKind::Session,
        },
        scope_key: metadata.scope_key.clone(),
    }
}

fn graph_scope_visible(scope: &GraphScope, authorization: &GraphAuthorization) -> bool {
    match scope.kind {
        GraphScopeKind::Session => {
            scope.scope_key.as_deref() == Some(authorization.session_scope_key.as_str())
        }
        GraphScopeKind::Agent => {
            scope.scope_key.as_deref() == Some(authorization.agent_scope_key.as_str())
        }
        GraphScopeKind::Project | GraphScopeKind::Repository => true,
    }
}

fn graph_scope_filter_matches(scope: &GraphScope, filter: Option<&GraphScopeFilter>) -> bool {
    let Some(filter) = filter else { return true };
    filter
        .memory_scope
        .as_deref()
        .is_none_or(|kind| kind == scope.kind.as_str())
        && filter
            .verified_scope_key
            .as_deref()
            .is_none_or(|key| key == scope.scope_key.as_deref().unwrap_or_default())
}

fn graph_time_filter_matches(
    relation: &StoredGraphRelation,
    filter: Option<&GraphTimeFilter>,
    current_ms: u64,
) -> bool {
    let exact_as_of = filter.and_then(graph_exact_as_of_ms);
    if !graph_relation_valid_at(relation, exact_as_of.unwrap_or(current_ms)) {
        return false;
    }
    let Some(filter) = filter else { return true };
    (exact_as_of.is_some()
        || (filter
            .valid_after_ms
            .is_none_or(|value| relation.valid_at_ms.is_some_and(|valid| valid >= value))
            && filter
                .valid_before_ms
                .is_none_or(|value| relation.valid_at_ms.is_some_and(|valid| valid <= value))))
        && filter
            .extracted_after_ms
            .is_none_or(|value| relation.extracted_at_ms >= value)
        && filter
            .extracted_before_ms
            .is_none_or(|value| relation.extracted_at_ms <= value)
}

fn graph_relation_valid_at(relation: &StoredGraphRelation, at_ms: u64) -> bool {
    relation.status == "active"
        && relation
            .valid_at_ms
            .is_none_or(|valid_at_ms| valid_at_ms <= at_ms)
        && relation
            .invalid_at_ms
            .is_none_or(|invalid_at_ms| at_ms < invalid_at_ms)
}

fn graph_exact_as_of_ms(filter: &GraphTimeFilter) -> Option<u64> {
    match (filter.valid_after_ms, filter.valid_before_ms) {
        (Some(after), Some(before)) if after == before => Some(after),
        _ => None,
    }
}

fn validate_graph_time_filter(filter: Option<&GraphTimeFilter>) -> Result<()> {
    let Some(filter) = filter else { return Ok(()) };
    ensure!(
        filter
            .valid_after_ms
            .zip(filter.valid_before_ms)
            .is_none_or(|(after, before)| after <= before),
        "graph valid-time range is invalid"
    );
    ensure!(
        filter
            .extracted_after_ms
            .zip(filter.extracted_before_ms)
            .is_none_or(|(after, before)| after <= before),
        "graph extraction-time range is invalid"
    );
    Ok(())
}

fn graph_job_visible(job: &GraphExtractionJob, authorization: &GraphAuthorization) -> bool {
    job.sources
        .iter()
        .all(|source| graph_scope_visible(&source.scope, authorization))
}

fn graph_job_source_binding(source: &crate::graph::GraphJobSource) -> GraphSourceBinding {
    GraphSourceBinding {
        source_memory_id: source.source_memory_id.clone(),
        source_unit_id: source.source_unit_id.clone(),
        content_hash: source.content_hash.clone(),
        extraction_revision: source.extraction_revision.clone(),
        derived_scope: Some(graph_derived_scope(&source.scope)),
        origin: source.origin.clone(),
        policy_revision: source.policy_revision.clone(),
        remote_eligible: source.remote_eligible,
    }
}

fn graph_job_units(
    job: &GraphExtractionJob,
    sources: &[GraphSource],
) -> Result<Vec<GraphExtractionUnit>> {
    ensure!(
        sources.len() == job.sources.len(),
        "graph job source count changed before claim"
    );
    let max_unit_bytes = job.max_unit_text_bytes as usize;
    let max_total_bytes = job.max_total_text_bytes as usize;
    let mut total_bytes = 0_usize;
    let mut units = Vec::with_capacity(sources.len());
    for source in sources {
        ensure!(
            source.remote_eligible,
            "graph source is no longer eligible for remote extraction"
        );
        let text = truncate_utf8(&source.text, max_unit_bytes);
        ensure!(
            total_bytes.saturating_add(text.len()) <= max_total_bytes,
            "graph job total text limit was exceeded"
        );
        total_bytes = total_bytes.saturating_add(text.len());
        units.push(GraphExtractionUnit {
            source: Some(graph_source_binding(source)),
            text,
            remote_ineligible_reason: None,
        });
    }
    Ok(units)
}

fn proto_graph_job(job: &GraphExtractionJob) -> crate::graph_proto::GraphExtractionJob {
    let state = match job.state {
        GraphJobState::Queued => GraphExtractionJobState::Queued,
        GraphJobState::Claimed => GraphExtractionJobState::Claimed,
        GraphJobState::Running => GraphExtractionJobState::Running,
        GraphJobState::Completed => GraphExtractionJobState::Completed,
        GraphJobState::Failed => GraphExtractionJobState::Failed,
        GraphJobState::Cancelled => GraphExtractionJobState::Cancelled,
    };
    crate::graph_proto::GraphExtractionJob {
        job_id: job.job_id.clone(),
        idempotency_digest: job.idempotency_digest.clone(),
        state: state as i32,
        sources: job.sources.iter().map(graph_job_source_binding).collect(),
        provider: Some(GraphProviderIdentity {
            provider_id: job.provider.provider_id.clone(),
            model_id: job.provider.model_id.clone(),
            extractor_version: job.provider.extractor_version.clone(),
            prompt_version: job.provider.prompt_version.clone(),
            schema_version: job.provider.schema_version.clone(),
            variant: job.provider.variant.clone(),
        }),
        attempt_count: job.attempt_count,
        max_attempts: job.max_attempts,
        created_at_ms: job.created_at_ms,
        updated_at_ms: job.updated_at_ms,
        lease_expires_at_ms: job.lease_expires_at_ms,
        extraction_run_id: job.extraction_run_id.clone(),
        next_attempt_at_ms: job.next_attempt_at_ms,
        cancel_requested: job.cancel_requested,
        error_code: job.error_code.clone(),
        error_message: job.error_message.clone(),
        max_unit_text_bytes: job.max_unit_text_bytes,
        max_total_text_bytes: job.max_total_text_bytes,
    }
}

fn proto_upsert_response(
    outcome: &crate::graph::GraphUpsertOutcome,
    entities: &[GraphEntityInput],
    relations: &[GraphRelationInput],
) -> GraphUpsertCandidatesResponse {
    GraphUpsertCandidatesResponse {
        receipt: Some(proto_run_receipt(&outcome.run)),
        accepted_entities: outcome
            .entities
            .iter()
            .map(|(index, entity)| GraphAcceptedEntity {
                candidate_index: *index as u32,
                entity_id: entity.entity_id.clone(),
                canonical_name: entity.canonical_name.clone(),
                entity_type: entity.entity_type.clone(),
                derived_scope: Some(graph_derived_scope(&entity.scope)),
                evidence: entities
                    .get(*index)
                    .map(|candidate| {
                        candidate
                            .evidence
                            .iter()
                            .map(proto_candidate_evidence)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect(),
        accepted_relations: outcome
            .relations
            .iter()
            .map(|(index, relation)| GraphAcceptedRelation {
                candidate_index: *index as u32,
                relation_id: relation.relation_id.clone(),
                subject_entity_id: relation.subject_entity_id.clone(),
                predicate: relation.predicate.clone(),
                object_entity_id: relation.object_entity_id.clone(),
                relation_type: relation.relation_type.clone(),
                derived_scope: Some(graph_derived_scope(&relation.scope)),
                evidence: relations
                    .get(*index)
                    .map(|candidate| {
                        candidate
                            .evidence
                            .iter()
                            .map(proto_candidate_evidence)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect(),
        rejected_candidates: outcome.rejected.iter().map(proto_rejection).collect(),
        conflicts: outcome.conflicts.iter().map(proto_conflict).collect(),
        warnings: outcome.warnings.clone(),
    }
}

fn graph_provider(provider: &GraphProviderIdentity) -> GraphProvider {
    GraphProvider {
        provider_id: provider.provider_id.clone(),
        model_id: provider.model_id.clone(),
        extractor_version: provider.extractor_version.clone(),
        prompt_version: provider.prompt_version.clone(),
        schema_version: provider.schema_version.clone(),
        variant: provider.variant.clone(),
    }
}

fn graph_entity_input(candidate: &GraphEntityCandidate) -> Result<GraphEntityInput> {
    Ok(GraphEntityInput {
        mention: candidate.mention.clone(),
        canonical_hint: candidate.canonical_hint.clone(),
        entity_type: candidate.entity_type.clone(),
        aliases: candidate.aliases.clone(),
        evidence: candidate
            .evidence
            .iter()
            .map(graph_evidence_input)
            .collect::<Result<Vec<_>>>()?,
        confidence: candidate.confidence,
    })
}

fn graph_relation_input(candidate: &GraphRelationCandidate) -> Result<GraphRelationInput> {
    Ok(GraphRelationInput {
        subject_mention: candidate.subject_mention.clone(),
        predicate: candidate.predicate.clone(),
        object_mention: candidate.object_mention.clone(),
        relation_type: candidate.relation_type.clone(),
        valid_at_ms: candidate.valid_at_ms,
        invalid_at_ms: candidate.invalid_at_ms,
        evidence: candidate
            .evidence
            .iter()
            .map(graph_evidence_input)
            .collect::<Result<Vec<_>>>()?,
        confidence: candidate.confidence,
    })
}

fn graph_evidence_input(evidence: &GraphCandidateEvidence) -> Result<GraphEvidenceInput> {
    ensure!(
        evidence.quote.chars().count() <= MAX_GRAPH_QUOTE_CHARS,
        "graph evidence quote is too long"
    );
    Ok(GraphEvidenceInput {
        source_unit_id: evidence.source_unit_id.clone(),
        quote: evidence.quote.clone(),
        utf8_start: evidence.utf8_start,
        utf8_end: evidence.utf8_end,
        occurrence_index: evidence.occurrence_index,
    })
}

fn graph_source_binding(source: &GraphSource) -> GraphSourceBinding {
    GraphSourceBinding {
        source_memory_id: source.source_memory_id.clone(),
        source_unit_id: source.source_unit_id.clone(),
        content_hash: source.content_hash.clone(),
        extraction_revision: source.extraction_revision.clone(),
        derived_scope: Some(graph_derived_scope(&source.scope)),
        origin: source.origin.clone(),
        policy_revision: source.policy_revision.clone(),
        remote_eligible: source.remote_eligible,
    }
}

fn graph_source_from_binding(binding: &GraphSourceBinding) -> Result<GraphSource> {
    let scope = binding
        .derived_scope
        .as_ref()
        .ok_or_else(|| anyhow!("graph source derived scope is required"))?;
    let kind = match scope.memory_scope.as_str() {
        "project" => GraphScopeKind::Project,
        "repository" => GraphScopeKind::Repository,
        "agent" => GraphScopeKind::Agent,
        "session" => GraphScopeKind::Session,
        _ => bail!("graph source memory scope is unknown"),
    };
    let scope_key = match kind {
        GraphScopeKind::Project | GraphScopeKind::Repository => {
            ensure!(
                scope.verified_scope_key.is_empty(),
                "project or repository graph scope cannot have a scope key"
            );
            None
        }
        GraphScopeKind::Agent | GraphScopeKind::Session => {
            ensure!(
                !scope.verified_scope_key.is_empty(),
                "agent or session graph scope requires a scope key"
            );
            Some(scope.verified_scope_key.clone())
        }
    };
    Ok(GraphSource {
        source_memory_id: binding.source_memory_id.clone(),
        source_unit_id: binding.source_unit_id.clone(),
        content_hash: binding.content_hash.clone(),
        extraction_revision: binding.extraction_revision.clone(),
        scope: GraphScope {
            project_id: scope.project_id.clone(),
            kind,
            scope_key,
        },
        origin: binding.origin.clone(),
        policy_revision: binding.policy_revision.clone(),
        remote_eligible: binding.remote_eligible,
        text: String::new(),
    })
}

fn graph_derived_scope(scope: &GraphScope) -> GraphDerivedScope {
    GraphDerivedScope {
        project_id: scope.project_id.clone(),
        memory_scope: scope.kind.as_str().to_string(),
        verified_scope_key: scope.scope_key.clone().unwrap_or_default(),
    }
}

fn proto_candidate_evidence(evidence: &GraphEvidenceInput) -> GraphCandidateEvidence {
    GraphCandidateEvidence {
        source_unit_id: evidence.source_unit_id.clone(),
        quote: evidence.quote.clone(),
        utf8_start: evidence.utf8_start,
        utf8_end: evidence.utf8_end,
        occurrence_index: evidence.occurrence_index,
    }
}

fn proto_candidate_evidence_from_graph(evidence: &GraphEvidence) -> GraphCandidateEvidence {
    GraphCandidateEvidence {
        source_unit_id: evidence.source_unit_id.clone(),
        quote: evidence.quote.clone(),
        utf8_start: evidence.utf8_start,
        utf8_end: evidence.utf8_end,
        occurrence_index: evidence.occurrence_index,
    }
}

fn proto_entity(entity: &StoredGraphEntity) -> GraphEntity {
    GraphEntity {
        entity_id: entity.entity_id.clone(),
        canonical_name: entity.canonical_name.clone(),
        entity_type: entity.entity_type.clone(),
        aliases: entity.aliases.iter().cloned().collect(),
        derived_scope: Some(graph_derived_scope(&entity.scope)),
        first_seen_at_ms: entity.first_seen_at_ms,
        last_seen_at_ms: entity.last_seen_at_ms,
        source_count: entity.source_count,
    }
}

fn proto_relation(relation: &StoredGraphRelation) -> GraphRelation {
    GraphRelation {
        relation_id: relation.relation_id.clone(),
        subject_entity_id: relation.subject_entity_id.clone(),
        predicate: relation.predicate.clone(),
        object_entity_id: relation.object_entity_id.clone(),
        relation_type: relation.relation_type.clone(),
        valid_at_ms: relation.valid_at_ms,
        invalid_at_ms: relation.invalid_at_ms,
        created_at_ms: relation.created_at_ms,
        extracted_at_ms: relation.extracted_at_ms,
        confidence: relation.confidence,
        status: relation.status.clone(),
        source_memory_ids: relation
            .evidence
            .iter()
            .map(|evidence| evidence.source_memory_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect(),
        evidence: relation
            .evidence
            .iter()
            .map(proto_candidate_evidence_from_graph)
            .collect(),
        extractor_version: relation.extractor_version.clone(),
        derived_scope: Some(graph_derived_scope(&relation.scope)),
    }
}

fn proto_run_receipt(run: &crate::graph::GraphRun) -> GraphRunReceipt {
    GraphRunReceipt {
        extraction_run_id: run.extraction_run_id.clone(),
        idempotency_digest: run.idempotency_digest.clone(),
        outcome: run.outcome.clone(),
        committed_at_ms: run.committed_at_ms,
        source_count: run.source_count,
        accepted_entity_count: run.accepted_entity_count,
        accepted_relation_count: run.accepted_relation_count,
        rejected_candidate_count: run.rejected_candidate_count,
        conflict_count: run.conflict_count,
        warning_count: run.warning_count,
        terminal: run.terminal,
    }
}

fn proto_rejection(rejection: &GraphCandidateRejection) -> ProtoGraphCandidateRejection {
    ProtoGraphCandidateRejection {
        candidate_kind: rejection.kind.clone(),
        candidate_index: rejection.index as u32,
        code: rejection.code.clone(),
        message: rejection.message.clone(),
    }
}

fn proto_conflict(conflict: &GraphCandidateConflict) -> ProtoGraphCandidateConflict {
    ProtoGraphCandidateConflict {
        candidate_kind: conflict.kind.clone(),
        candidate_index: conflict.index as u32,
        existing_id: conflict.existing_id.clone(),
        code: conflict.code.clone(),
        message: conflict.message.clone(),
    }
}

fn source_error_code(error: &anyhow::Error) -> String {
    let message = error.to_string();
    if message.contains("not found") {
        "not_found".to_string()
    } else if message.contains("expired") {
        "expired".to_string()
    } else if message.contains("stale") {
        "stale".to_string()
    } else if message.contains("visible") {
        "not_found".to_string()
    } else {
        "ineligible".to_string()
    }
}

fn source_error_message(error: &anyhow::Error) -> String {
    if error.to_string().contains("visible") {
        "memory not found".to_string()
    } else {
        error.to_string()
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn graph_source_unit_id(
    id: &str,
    content_hash: &str,
    scope: &GraphScope,
    origin: &MemoryOrigin,
    source: &str,
) -> String {
    let material = format!(
        "{}\0{}\0{}\0{}\0{}",
        id,
        content_hash,
        scope.kind.as_str(),
        memory_origin_name(*origin),
        source
    );
    format!("unit_{}", &hash_hex(material.as_bytes())[..32])
}

fn graph_revision(document: &Doc, metadata: &MemoryMetadata) -> String {
    let content_hash = document
        .get_string("content_hash")
        .ok()
        .flatten()
        .unwrap_or_default();
    graph_pending_revision_parts(
        &content_hash,
        metadata,
        document
            .get_string("source")
            .ok()
            .flatten()
            .as_deref()
            .unwrap_or_default(),
    )
}

fn graph_pending_revision(document: &PendingDocument, metadata: &MemoryMetadata) -> String {
    graph_pending_revision_parts(&document.content_hash, metadata, &document.source)
}

fn graph_pending_revision_parts(
    content_hash: &str,
    metadata: &MemoryMetadata,
    source: &str,
) -> String {
    hash_hex(
        format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            content_hash,
            metadata.scope.as_str(),
            metadata.scope_key.as_deref().unwrap_or_default(),
            memory_origin_name(metadata.origin),
            source,
            GRAPH_POLICY_VERSION
        )
        .as_bytes(),
    )
}

fn memory_origin_name(origin: MemoryOrigin) -> &'static str {
    match origin {
        MemoryOrigin::Manual => "manual",
        MemoryOrigin::AutoCompaction => "auto_compaction",
        MemoryOrigin::SharedMarkdown => "shared_markdown",
        MemoryOrigin::IngestedDocument => "ingested_document",
        MemoryOrigin::Legacy => "legacy",
    }
}

fn graph_remote_ineligible_reason(source: &GraphSource) -> Option<String> {
    if !source.remote_eligible {
        Some(graph_remote_ineligible_reason_text(
            &source.origin,
            &source.text,
        ))
    } else {
        None
    }
}

fn graph_remote_ineligible_reason_values(
    stored: &StoredMemory,
    metadata: &MemoryMetadata,
) -> Option<String> {
    if metadata.scope == MemoryScope::Repository {
        return Some(
            "repository-scoped memory requires explicit local extraction policy".to_string(),
        );
    }
    if !metadata.code_anchors.is_empty() || source_is_code(&stored.source) {
        return Some("code-backed memory is blocked from remote extraction by default".to_string());
    }
    let request = StoreRequest {
        content: stored.content.clone(),
        title: Some(stored.title.clone()),
        kind: stored.kind,
        importance: stored.importance,
        tags: stored.tags.clone(),
        source: Some(stored.source.clone()),
        scope: metadata.scope,
        scope_key: metadata.scope_key.clone(),
        origin: metadata.origin,
        expires_in_days: None,
        code_paths: Vec::new(),
        revive: false,
        taxonomy: None,
        confidence: None,
    };
    (!matches!(
        classify_capture_safety(&request, SourceTrust::User, true),
        CaptureSafety::Safe
    ))
    .then(|| {
        "secret-like or prompt-injection-shaped content is blocked from remote extraction"
            .to_string()
    })
}

fn graph_remote_ineligible_reason_text(origin: &str, text: &str) -> String {
    if origin == "repository" {
        "repository-scoped memory requires explicit local extraction policy".to_string()
    } else if text.is_empty() {
        "empty source text".to_string()
    } else {
        "source egress policy blocks remote extraction".to_string()
    }
}

fn source_is_code(source: &str) -> bool {
    [
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".c", ".cpp", ".h", ".hpp",
        ".cs", ".swift", ".kt", ".kts", ".sql", ".sh", ".yaml", ".yml", ".json",
    ]
    .iter()
    .any(|suffix| source.to_ascii_lowercase().contains(suffix))
}

fn now_ms_u64() -> Result<u64> {
    u64::try_from(now_ms()?).context("graph timestamp cannot be negative")
}

fn stored_len_exceeded(
    memories: &[GraphMemorySearchResult],
    entities: &[GraphEntitySearchResult],
    relations: &[GraphRelationSearchResult],
    max: usize,
) -> bool {
    memories.len() >= max || entities.len() >= max || relations.len() >= max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_scope() -> GraphScope {
        GraphScope {
            project_id: "project".to_string(),
            kind: GraphScopeKind::Project,
            scope_key: None,
        }
    }

    fn test_evidence(index: usize) -> GraphEvidence {
        GraphEvidence {
            source_memory_id: format!("mem-{index}"),
            source_unit_id: format!("unit-{index}"),
            content_hash: "a".repeat(64),
            extraction_revision: "r1".to_string(),
            scope: test_scope(),
            quote: format!("evidence-{index}"),
            occurrence_index: 0,
            utf8_start: None,
            utf8_end: None,
        }
    }

    fn test_relation(valid_at_ms: Option<u64>, invalid_at_ms: Option<u64>) -> StoredGraphRelation {
        StoredGraphRelation {
            relation_id: "rel-1".to_string(),
            subject_entity_id: "subject".to_string(),
            predicate: "uses".to_string(),
            object_entity_id: "object".to_string(),
            relation_type: "dependency".to_string(),
            valid_at_ms,
            invalid_at_ms,
            created_at_ms: 1,
            extracted_at_ms: 150,
            confidence: 1.0,
            status: "active".to_string(),
            evidence: vec![test_evidence(0)],
            extractor_version: "extractor-v1".to_string(),
            scope: test_scope(),
        }
    }

    #[test]
    fn temporal_current_and_historical_boundaries_are_start_inclusive_end_exclusive() {
        let relation = test_relation(Some(100), Some(200));

        assert!(!graph_time_filter_matches(&relation, None, 99));
        assert!(graph_time_filter_matches(&relation, None, 100));
        assert!(graph_time_filter_matches(&relation, None, 199));
        assert!(!graph_time_filter_matches(&relation, None, 200));

        let at_start = GraphTimeFilter {
            valid_after_ms: Some(100),
            valid_before_ms: Some(100),
            ..GraphTimeFilter::default()
        };
        let at_end = GraphTimeFilter {
            valid_after_ms: Some(200),
            valid_before_ms: Some(200),
            ..GraphTimeFilter::default()
        };
        assert!(graph_time_filter_matches(&relation, Some(&at_start), 250));
        assert!(!graph_time_filter_matches(&relation, Some(&at_end), 150));
    }

    #[test]
    fn active_evidence_is_filtered_before_the_output_limit() {
        let evidence = (0..10).map(test_evidence).collect::<Vec<_>>();

        let active = select_active_evidence(&evidence, 1, |item| {
            item.source_memory_id == "mem-8" || item.source_memory_id == "mem-9"
        });

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].source_memory_id, "mem-8");
    }

    #[test]
    fn inverted_temporal_ranges_are_rejected() {
        let invalid = GraphTimeFilter {
            valid_after_ms: Some(200),
            valid_before_ms: Some(100),
            ..GraphTimeFilter::default()
        };

        assert!(validate_graph_time_filter(Some(&invalid)).is_err());
    }
}
