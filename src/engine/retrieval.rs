//! Hybrid dense/lexical retrieval and score calibration.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use zvec_rust::{Doc, Fts, SearchQuery};

use super::{decorate_memory, now_ms, stored_memory_from_doc, MemoryEngine, StoredMemory};
use crate::config::hash_hex;
use crate::contract::{
    MemoryKind, MemoryRecord, MemoryScope, ScoreBreakdown, SearchRequest, SearchResponse,
};
use crate::lifecycle::{is_expired, retention_factor};
use crate::storage::state::{MemoryMetadata, RetrievalRecord};
use crate::storage::zvec::RESULT_FIELDS;
use crate::validation::{
    truncate_chars, validate_search_request, MAX_BUDGET_CHARS, MAX_SEARCH_RESULTS, MIN_BUDGET_CHARS,
};

const SCORE_VERSION: &str = "hybrid_v3_taxonomy";
const MAX_EXCERPT_CHARS: usize = 1_600;
const MAX_CANDIDATES: usize = 1_000;
const ABSTENTION_THRESHOLD: f32 = 0.42;

impl MemoryEngine {
    /// Search dense and lexical channels, calibrate scores, apply lifecycle
    /// filters, diversify with MMR, and pack results into a character budget.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input or a storage/inference failure.
    pub fn search(&mut self, request: &SearchRequest) -> Result<SearchResponse> {
        validate_search_request(request)?;
        let query_text = request.query.trim().to_string();
        let budget_chars = request
            .budget_chars
            .clamp(MIN_BUDGET_CHARS, MAX_BUDGET_CHARS);
        let max_results = request
            .limit
            .unwrap_or(request.max_results)
            .clamp(1, MAX_SEARCH_RESULTS);
        let stats = self.collection.stats()?;
        if stats.doc_count == 0 {
            return Ok(empty_search_response(
                query_text,
                budget_chars,
                "empty_store",
            ));
        }

        let query_embedding = self.embed(&format!("query: {query_text}"))?;
        let candidate_count = usize::try_from(stats.doc_count)
            .unwrap_or(MAX_CANDIDATES)
            .min(MAX_CANDIDATES);
        let filter = kind_filter(&request.kinds);
        let dense_documents =
            self.dense_query(&query_embedding, candidate_count, filter.as_deref())?;
        let lexical_documents = self
            .lexical_query(&query_text, candidate_count, filter.as_deref())
            .unwrap_or_default();
        let candidates = merge_candidates(&dense_documents, &lexical_documents)?;
        let considered = candidates.len();
        let now = now_ms()?;
        let (ranked, mut state_dirty) = self.rank_candidates(candidates, request, &query_text, now);

        let ranked = deduplicate_layers(ranked);
        let (memories, used_chars) = select_mmr(ranked, max_results, budget_chars);
        let abstained = memories.is_empty();
        let abstention_reason = abstained.then(|| {
            if considered == 0 {
                "no_candidates".to_string()
            } else {
                "low_relevance_or_ineligible".to_string()
            }
        });
        let retrieval_id = if request.track_feedback && !memories.is_empty() {
            let id = retrieval_id(&query_text, now, self.state.generation, &memories);
            self.state.retrievals.insert(
                id.clone(),
                RetrievalRecord {
                    query_hash: hash_hex(query_text.as_bytes()),
                    memory_ids: memories.iter().map(|memory| memory.id.clone()).collect(),
                    created_at_ms: now,
                    events: Vec::new(),
                    event_memory_ids: HashMap::new(),
                },
            );
            state_dirty = true;
            Some(id)
        } else {
            None
        };
        if state_dirty {
            self.save_state()?;
        }

        Ok(SearchResponse {
            query: query_text,
            retrieval_id,
            count: memories.len(),
            candidates_considered: considered,
            budget_chars,
            used_chars,
            abstained,
            abstention_reason,
            score_version: SCORE_VERSION,
            memories,
        })
    }

    fn rank_candidates(
        &mut self,
        candidates: HashMap<String, RetrievalCandidate>,
        request: &SearchRequest,
        query_text: &str,
        now: i64,
    ) -> (Vec<RankedMemory>, bool) {
        let mut ranked = Vec::with_capacity(candidates.len());
        let mut state_dirty = false;
        for candidate in candidates.into_values() {
            if self.state.pending_deletes.contains(&candidate.memory.id) {
                continue;
            }
            let metadata = self.state.metadata(
                &candidate.memory.id,
                candidate.memory.kind,
                candidate.memory.created_at_ms,
                candidate.memory.importance,
            );
            if !self.state.records.contains_key(&candidate.memory.id) {
                self.state
                    .records
                    .insert(candidate.memory.id.clone(), metadata.clone());
                state_dirty = true;
            }
            if !scope_visible(&metadata, request) || is_expired(&metadata, now) {
                continue;
            }
            // Phase 1: exclude superseded memories unless explicitly requested.
            if !request.include_superseded && metadata.is_superseded() {
                continue;
            }
            // Phase 1: taxonomy filter.
            if !request.taxonomies.is_empty() && !request.taxonomies.contains(&metadata.taxonomy) {
                continue;
            }
            let stale = crate::validation::anchors_stale(&self.config, &metadata.code_anchors);
            if stale && !request.include_stale {
                continue;
            }
            let lexical = lexical_score(
                query_text,
                &candidate.memory.title,
                &candidate.memory.content,
                &candidate.memory.tags,
            );
            let dense = candidate.dense_similarity.unwrap_or_default();
            let reciprocal_rank =
                normalized_reciprocal_rank(candidate.dense_rank, candidate.lexical_rank);
            let channel_agreement =
                f32::from(candidate.dense_rank.is_some() && candidate.lexical_rank.is_some());
            // Phase 1: per-candidate retrieval profile weights from taxonomy.
            let (w_dense, w_rr, w_lex, w_agree) = metadata.taxonomy.retrieval_profile().weights();
            let raw = w_dense * dense
                + w_rr * reciprocal_rank
                + w_lex * lexical
                + w_agree * channel_agreement;
            let calibrated = logistic(10.0 * (raw - 0.55));
            let retention = retention_factor(now, candidate.memory.updated_at_ms, &metadata);
            let feedback = feedback_factor(&metadata.feedback);
            // Phase 1: quality modifier .9 + .1 * ((importance + confidence) / 2).
            let quality =
                0.9 + 0.1 * f32::midpoint(candidate.memory.importance, metadata.confidence);
            let score = (calibrated * retention * feedback * quality).clamp(0.0, 1.0);
            if score < request.min_score.max(ABSTENTION_THRESHOLD) {
                continue;
            }
            let mut memory = decorate_memory(candidate.memory, metadata, stale);
            memory.content = truncate_chars(&memory.content, MAX_EXCERPT_CHARS);
            memory.score = Some(score);
            memory.score_breakdown = Some(ScoreBreakdown {
                dense,
                reciprocal_rank,
                lexical,
                channel_agreement,
                calibrated,
                retention,
                feedback,
            });
            ranked.push(RankedMemory { memory, score });
        }
        (ranked, state_dirty)
    }

    fn dense_query(
        &self,
        embedding: &[f32],
        candidate_count: usize,
        filter: Option<&str>,
    ) -> Result<Vec<Doc>> {
        let mut query = SearchQuery::new("embedding", embedding, i32::try_from(candidate_count)?)?;
        query.set_output_fields(&RESULT_FIELDS)?;
        if let Some(filter) = filter {
            query.set_filter(filter)?;
        }
        Ok(self.collection.query(&query)?)
    }

    fn lexical_query(
        &self,
        query_text: &str,
        candidate_count: usize,
        filter: Option<&str>,
    ) -> Result<Vec<Doc>> {
        let mut fts = Fts::new()?;
        fts.set_match_string(query_text)?;
        let mut query = SearchQuery::fts("search_text", &fts, i32::try_from(candidate_count)?)?;
        query.set_output_fields(&RESULT_FIELDS)?;
        if let Some(filter) = filter {
            query.set_filter(filter)?;
        }
        Ok(self.collection.query(&query)?)
    }
}

struct RetrievalCandidate {
    memory: StoredMemory,
    dense_rank: Option<usize>,
    lexical_rank: Option<usize>,
    dense_similarity: Option<f32>,
}

struct RankedMemory {
    memory: MemoryRecord,
    score: f32,
}

fn merge_candidates(dense: &[Doc], lexical: &[Doc]) -> Result<HashMap<String, RetrievalCandidate>> {
    let mut candidates = HashMap::new();
    for (rank, document) in dense.iter().enumerate() {
        let memory = stored_memory_from_doc(document)?;
        let id = memory.id.clone();
        candidates.insert(
            id,
            RetrievalCandidate {
                memory,
                dense_rank: Some(rank),
                lexical_rank: None,
                dense_similarity: Some(f32::midpoint(document.get_score(), 1.0).clamp(0.0, 1.0)),
            },
        );
    }
    for (rank, document) in lexical.iter().enumerate() {
        let memory = stored_memory_from_doc(document)?;
        candidates
            .entry(memory.id.clone())
            .and_modify(|candidate| candidate.lexical_rank = Some(rank))
            .or_insert(RetrievalCandidate {
                memory,
                dense_rank: None,
                lexical_rank: Some(rank),
                dense_similarity: None,
            });
    }
    Ok(candidates)
}

fn normalized_reciprocal_rank(dense_rank: Option<usize>, lexical_rank: Option<usize>) -> f32 {
    fn channel(rank: Option<usize>, weight: f32) -> f32 {
        rank.map_or(0.0, |rank| {
            let rank = f32::from(u16::try_from(rank).unwrap_or(u16::MAX));
            weight * 61.0 / (61.0 + rank)
        })
    }
    (channel(dense_rank, 0.65) + channel(lexical_rank, 0.35)).clamp(0.0, 1.0)
}

fn logistic(value: f32) -> f32 {
    1.0 / (1.0 + (-value).exp())
}

fn feedback_factor(feedback: &crate::contract::FeedbackStats) -> f32 {
    if feedback.injected < 3 {
        return 1.0;
    }
    let denominator = bounded_u64_f32(feedback.injected.max(1));
    let signal = (bounded_u64_f32(feedback.used)
        - bounded_u64_f32(feedback.ignored)
        - bounded_u64_f32(feedback.error))
        / denominator;
    (1.0 + 0.1 * signal).clamp(0.9, 1.1)
}

fn scope_visible(metadata: &MemoryMetadata, request: &SearchRequest) -> bool {
    if !request.scopes.is_empty() && !request.scopes.contains(&metadata.scope) {
        return false;
    }
    match metadata.scope {
        MemoryScope::Session => metadata.scope_key == request.session_scope_key,
        MemoryScope::Agent => metadata.scope_key == request.agent_scope_key,
        MemoryScope::Project | MemoryScope::Repository => true,
    }
}

fn deduplicate_layers(ranked: Vec<RankedMemory>) -> Vec<RankedMemory> {
    let mut deduplicated: HashMap<String, RankedMemory> = HashMap::new();
    for candidate in ranked {
        let key = hash_hex(
            format!(
                "{}\0{}",
                candidate.memory.kind.as_str(),
                candidate
                    .memory
                    .content
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .as_bytes(),
        );
        deduplicated
            .entry(key)
            .and_modify(|existing| {
                let candidate_priority = (
                    candidate.memory.scope.precedence(),
                    candidate.score,
                    candidate.memory.updated_at_ms,
                );
                let existing_priority = (
                    existing.memory.scope.precedence(),
                    existing.score,
                    existing.memory.updated_at_ms,
                );
                if candidate_priority > existing_priority {
                    *existing = RankedMemory {
                        memory: candidate.memory.clone(),
                        score: candidate.score,
                    };
                }
            })
            .or_insert(candidate);
    }
    deduplicated.into_values().collect()
}

fn select_mmr(
    mut candidates: Vec<RankedMemory>,
    max_results: usize,
    budget_chars: usize,
) -> (Vec<MemoryRecord>, usize) {
    let mut selected: Vec<MemoryRecord> = Vec::new();
    let mut used_chars: usize = 0;
    while !candidates.is_empty() && selected.len() < max_results {
        let best = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let similarity = selected
                    .iter()
                    .map(|memory| memory_similarity(&candidate.memory, memory))
                    .fold(0.0_f32, f32::max);
                let mmr = 0.75 * candidate.score - 0.25 * similarity;
                (index, mmr, candidate.score)
            })
            .max_by(|left, right| {
                left.1
                    .total_cmp(&right.1)
                    .then_with(|| left.2.total_cmp(&right.2))
            });
        let Some((index, _, _)) = best else {
            break;
        };
        let candidate = candidates.swap_remove(index);
        let estimated = estimate_memory_chars(&candidate.memory);
        if used_chars.saturating_add(estimated) > budget_chars {
            continue;
        }
        used_chars += estimated;
        selected.push(candidate.memory);
    }
    (selected, used_chars)
}

fn memory_similarity(left: &MemoryRecord, right: &MemoryRecord) -> f32 {
    let left_tokens = tokens(&format!(
        "{} {} {}",
        left.title,
        left.tags.join(" "),
        left.content
    ));
    let right_tokens = tokens(&format!(
        "{} {} {}",
        right.title,
        right.tags.join(" "),
        right.content
    ));
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    let intersection = bounded_usize_f32(left_tokens.intersection(&right_tokens).count());
    let union = bounded_usize_f32(left_tokens.union(&right_tokens).count());
    intersection / union.max(1.0)
}

fn estimate_memory_chars(memory: &MemoryRecord) -> usize {
    memory.title.chars().count()
        + memory.content.chars().count()
        + memory
            .tags
            .iter()
            .map(|tag| tag.chars().count())
            .sum::<usize>()
        + 320
}

fn retrieval_id(query: &str, now: i64, generation: u64, memories: &[MemoryRecord]) -> String {
    let ids = memories
        .iter()
        .map(|memory| memory.id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let hash = hash_hex(format!("{query}\0{now}\0{generation}\0{ids}").as_bytes());
    format!("ret_{}", &hash[..24])
}

fn empty_search_response(query: String, budget_chars: usize, reason: &str) -> SearchResponse {
    SearchResponse {
        query,
        retrieval_id: None,
        count: 0,
        candidates_considered: 0,
        budget_chars,
        used_chars: 0,
        abstained: true,
        abstention_reason: Some(reason.to_string()),
        score_version: SCORE_VERSION,
        memories: Vec::new(),
    }
}

fn lexical_score(query: &str, title: &str, content: &str, tags: &[String]) -> f32 {
    let query_lower = query.to_lowercase();
    let haystack = format!("{}\n{}\n{}", title, tags.join(" "), content).to_lowercase();
    if haystack.contains(&query_lower) {
        return 1.0;
    }
    let query_tokens = tokens(&query_lower);
    if query_tokens.is_empty() {
        return 0.0;
    }
    let document_tokens = tokens(&haystack);
    let matches = bounded_usize_f32(query_tokens.intersection(&document_tokens).count());
    matches / bounded_usize_f32(query_tokens.len().max(1))
}

fn bounded_usize_f32(value: usize) -> f32 {
    f32::from(u16::try_from(value).unwrap_or(u16::MAX))
}

fn bounded_u64_f32(value: u64) -> f32 {
    f32::from(u16::try_from(value).unwrap_or(u16::MAX))
}

fn tokens(value: &str) -> HashSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.chars().count() >= 2)
        .map(str::to_lowercase)
        .collect()
}

fn kind_filter(kinds: &[MemoryKind]) -> Option<String> {
    if kinds.is_empty() {
        return None;
    }
    Some(
        kinds
            .iter()
            .map(|kind| format!("kind = '{}'", kind.as_str()))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        deduplicate_layers, kind_filter, lexical_score, logistic, normalized_reciprocal_rank,
        RankedMemory,
    };
    use crate::contract::{FeedbackStats, MemoryKind, MemoryOrigin, MemoryRecord, MemoryScope};

    #[test]
    fn lexical_overlap_handles_code_identifiers_and_vietnamese() {
        let score = lexical_score(
            "Rust memory",
            "Bộ nhớ native",
            "Dùng Rust cho opencode_memory sidecar",
            &["zvec".to_string()],
        );
        assert!(score > 0.9);
    }

    #[test]
    fn calibrated_score_components_are_bounded_and_monotonic() {
        let dense_only = normalized_reciprocal_rank(Some(0), None);
        let both = normalized_reciprocal_rank(Some(0), Some(0));
        assert!(dense_only > 0.0 && dense_only < both);
        assert!(both <= 1.0);
        assert!(logistic(-5.0) < logistic(0.0));
        assert!(logistic(0.0) < logistic(5.0));
    }

    #[test]
    fn higher_scope_wins_exact_cross_layer_duplicate() {
        fn memory(id: &str, scope: MemoryScope) -> MemoryRecord {
            MemoryRecord {
                id: id.to_string(),
                title: "Rust".to_string(),
                content: "Use Rust".to_string(),
                kind: MemoryKind::Decision,
                importance: 0.8,
                tags: Vec::new(),
                source: "test".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
                scope,
                origin: MemoryOrigin::Manual,
                expires_at_ms: None,
                pinned: false,
                locked: false,
                lock_reason: None,
                stale: false,
                code_anchors: Vec::new(),
                feedback: FeedbackStats::default(),
                taxonomy: crate::taxonomy::MemoryTaxonomy::Decision,
                confidence: 0.8,
                superseded_by: None,
                supersedes: Vec::new(),
                conflict_with: Vec::new(),
                score: Some(0.8),
                score_breakdown: None,
            }
        }
        let result = deduplicate_layers(vec![
            RankedMemory {
                memory: memory(
                    "mem_00000000000000000000000000000000",
                    MemoryScope::Repository,
                ),
                score: 0.9,
            },
            RankedMemory {
                memory: memory("mem_11111111111111111111111111111111", MemoryScope::Session),
                score: 0.7,
            },
        ]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].memory.scope, MemoryScope::Session);
    }

    #[test]
    fn kind_filter_only_uses_known_enum_values() {
        assert_eq!(
            kind_filter(&[MemoryKind::Decision, MemoryKind::Gotcha]),
            Some("kind = 'decision' OR kind = 'gotcha'".to_string())
        );
    }
}
