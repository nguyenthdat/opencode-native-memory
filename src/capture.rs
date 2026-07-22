//! Phase 1 capture gate: pure deterministic capture policy foundation.
//!
//! This module is deliberately side-effect free. It contains no RPC handler,
//! no LLM call, no plugin hook, no model invocation, and no Phase 2 learning.
//! It provides only the typed signal/decision enums and a deterministic
//! `CaptureGate::evaluate` function that maps `CaptureSignals` to a
//! `CaptureDecision`. The engine and RPC layers will compose this with I/O in
//! later phases.
//!
//! All floats are validated to be finite and in `[0.0, 1.0]` before any
//! arithmetic is performed; non-finite or out-of-range inputs return an error
//! instead of producing a silently broken decision.

use serde::{Deserialize, Serialize};

use anyhow::{bail, ensure, Result};

/// Default significance threshold below which a candidate is skipped.
pub const DEFAULT_SIGNIFICANCE_THRESHOLD: f32 = 0.5;

/// Default actionability threshold below which a candidate is skipped.
pub const DEFAULT_ACTIONABILITY_THRESHOLD: f32 = 0.5;

/// Confidence cap applied to auto-compacted captures.
pub const AUTO_COMPACTION_CONFIDENCE_CAP: f32 = 0.6;

/// Weight of `importance` in the actionability blend.
pub const ACTIONABILITY_IMPORTANCE_WEIGHT: f32 = 0.4;
/// Weight of `impact` in the actionability blend.
pub const ACTIONABILITY_IMPACT_WEIGHT: f32 = 0.3;
/// Weight of `rarity` in the actionability blend.
pub const ACTIONABILITY_RARITY_WEIGHT: f32 = 0.3;

/// Maximum allowed size for suggested supersession/conflict ID lists.
pub const MAX_SUGGESTED_RELATION_IDS: usize = 64;

/// Strongly-typed source trust classification.
///
/// `Untrusted` content must present valid evidence before a claim can be
/// accepted; otherwise the gate quarantines the candidate.
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceTrust {
    /// Authoritative curated source (e.g. reviewed repository Markdown).
    Authoritative,
    /// Direct user instruction stored manually.
    #[default]
    User,
    /// Agent-generated content that has not yet been verified.
    Agent,
    /// Untrusted content (auto-compacted, shared, or external).
    Untrusted,
}

/// Strongly-typed capture safety classification.
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSafety {
    /// Content passed all secret and injection scans.
    #[default]
    Safe,
    /// Content contains a likely secret (token, key, credential).
    SecretLeak,
    /// Content looks like prompt/instruction injection.
    Injection,
    /// Untrusted content asserts a claim without supporting evidence.
    UntrustedClaim,
}

/// Strongly-typed novelty disposition relative to the existing memory store.
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoveltyDisposition {
    /// The candidate is new and conflicts with no existing memory.
    #[default]
    Novel,
    /// The candidate duplicates an existing active memory.
    Duplicate,
    /// The candidate supersedes one or more existing memories.
    Supersedes,
    /// The candidate conflicts with one or more existing memories.
    Conflicts,
}

/// Typed capture signals supplied by the caller or upstream pipeline.
///
/// All float fields must be finite and in `[0.0, 1.0]`. The gate validates
/// this before any arithmetic.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSignals {
    /// Caller-supplied significance score in `[0, 1]`.
    pub significance: f32,
    /// `importance` component of actionability in `[0, 1]`.
    pub importance: f32,
    /// `impact` component of actionability in `[0, 1]`.
    pub impact: f32,
    /// `rarity` component of actionability in `[0, 1]`.
    pub rarity: f32,
    /// Trust classification of the source.
    #[serde(default)]
    pub source_trust: SourceTrust,
    /// Safety classification of the raw content.
    #[serde(default)]
    pub safety: CaptureSafety,
    /// Novelty disposition relative to existing memories.
    #[serde(default)]
    pub novelty: NoveltyDisposition,
    /// Whether the caller provided valid evidence for an untrusted claim.
    #[serde(default)]
    pub has_valid_evidence: bool,
    /// IDs of existing memories this candidate would supersede.
    #[serde(default)]
    pub suggested_supersession_ids: Vec<String>,
    /// IDs of existing memories this candidate conflicts with.
    #[serde(default)]
    pub suggested_conflict_ids: Vec<String>,
}

/// Strongly-typed reason a candidate was skipped.
#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Significance score below the configured threshold.
    LowSignificance,
    /// Actionability blend below the configured threshold.
    LowActionability,
    /// Candidate duplicates an existing active memory.
    Duplicate,
}

/// Strongly-typed reason a candidate was quarantined.
#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineReason {
    /// Content contains a likely secret.
    SecretLeak,
    /// Content looks like prompt/instruction injection.
    Injection,
    /// Untrusted source asserts a claim without valid evidence.
    UntrustedClaimWithoutEvidence,
}

/// Final capture decision.
#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CaptureDecision {
    /// The candidate was skipped and will not be stored.
    Skip {
        reason: SkipReason,
        significance: f32,
        actionability: f32,
    },
    /// The candidate was quarantined for manual review.
    Quarantine { reason: QuarantineReason },
    /// The candidate was accepted and may be stored.
    Accept(CapturePlan),
}

/// The concrete capture plan produced by an `Accept` decision.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapturePlan {
    /// Confidence to assign to the stored memory in `[0, 1]`.
    pub confidence: f32,
    /// IDs of memories this capture should supersede (verified by the engine).
    #[serde(default)]
    pub superseded_ids: Vec<String>,
    /// IDs of memories this capture conflicts with (verified by the engine).
    #[serde(default)]
    pub conflict_ids: Vec<String>,
    /// Whether the confidence was capped by the auto-compaction policy.
    #[serde(default)]
    pub confidence_capped: bool,
}

/// Pure deterministic capture gate.
///
/// The gate holds only configuration thresholds; it performs no I/O and no
/// model calls. `evaluate` is a pure function of `CaptureSignals`.
#[derive(Debug, Clone, Copy)]
pub struct CaptureGate {
    significance_threshold: f32,
    actionability_threshold: f32,
    auto_compaction_confidence_cap: f32,
}

impl Default for CaptureGate {
    fn default() -> Self {
        Self::new(
            DEFAULT_SIGNIFICANCE_THRESHOLD,
            DEFAULT_ACTIONABILITY_THRESHOLD,
            AUTO_COMPACTION_CONFIDENCE_CAP,
        )
    }
}

impl CaptureGate {
    /// Create a gate with custom thresholds.
    ///
    /// # Errors
    ///
    /// Returns an error if any threshold is non-finite or outside `[0, 1]`.
    #[must_use]
    pub fn new(
        significance_threshold: f32,
        actionability_threshold: f32,
        auto_compaction_confidence_cap: f32,
    ) -> Self {
        debug_assert!(significance_threshold.is_finite());
        debug_assert!(actionability_threshold.is_finite());
        debug_assert!(auto_compaction_confidence_cap.is_finite());
        Self {
            significance_threshold,
            actionability_threshold,
            auto_compaction_confidence_cap,
        }
    }

    /// Significance threshold.
    #[must_use]
    pub const fn significance_threshold(self) -> f32 {
        self.significance_threshold
    }

    /// Actionability threshold.
    #[must_use]
    pub const fn actionability_threshold(self) -> f32 {
        self.actionability_threshold
    }

    /// Auto-compaction confidence cap.
    #[must_use]
    pub const fn auto_compaction_confidence_cap(self) -> f32 {
        self.auto_compaction_confidence_cap
    }

    /// Evaluate `signals` and produce a deterministic capture decision.
    ///
    /// # Errors
    ///
    /// Returns an error if any float field of `signals` is non-finite or
    /// outside `[0, 1]`, or if the suggested relation ID lists are oversized.
    pub fn evaluate(&self, signals: CaptureSignals) -> Result<CaptureDecision> {
        validate_unit_finite("significance", signals.significance)?;
        validate_unit_finite("importance", signals.importance)?;
        validate_unit_finite("impact", signals.impact)?;
        validate_unit_finite("rarity", signals.rarity)?;
        ensure!(
            signals.suggested_supersession_ids.len() <= MAX_SUGGESTED_RELATION_IDS,
            "suggested_supersession_ids exceeds {MAX_SUGGESTED_RELATION_IDS} entries"
        );
        ensure!(
            signals.suggested_conflict_ids.len() <= MAX_SUGGESTED_RELATION_IDS,
            "suggested_conflict_ids exceeds {MAX_SUGGESTED_RELATION_IDS} entries"
        );

        // Safety quarantine takes priority over every other signal.
        match signals.safety {
            CaptureSafety::SecretLeak => {
                return Ok(CaptureDecision::Quarantine {
                    reason: QuarantineReason::SecretLeak,
                });
            }
            CaptureSafety::Injection => {
                return Ok(CaptureDecision::Quarantine {
                    reason: QuarantineReason::Injection,
                });
            }
            CaptureSafety::UntrustedClaim => {
                if signals.source_trust == SourceTrust::Untrusted && !signals.has_valid_evidence {
                    return Ok(CaptureDecision::Quarantine {
                        reason: QuarantineReason::UntrustedClaimWithoutEvidence,
                    });
                }
            }
            CaptureSafety::Safe => {}
        }

        // Duplicates are always skipped regardless of score.
        if signals.novelty == NoveltyDisposition::Duplicate {
            let actionability = blend_actionability(&signals);
            return Ok(CaptureDecision::Skip {
                reason: SkipReason::Duplicate,
                significance: signals.significance,
                actionability,
            });
        }

        if signals.significance < self.significance_threshold {
            let actionability = blend_actionability(&signals);
            return Ok(CaptureDecision::Skip {
                reason: SkipReason::LowSignificance,
                significance: signals.significance,
                actionability,
            });
        }

        let actionability = blend_actionability(&signals);
        if actionability < self.actionability_threshold {
            return Ok(CaptureDecision::Skip {
                reason: SkipReason::LowActionability,
                significance: signals.significance,
                actionability,
            });
        }

        let (raw_confidence, confidence_capped) = if signals.source_trust == SourceTrust::Untrusted
        {
            (
                signals.importance.min(self.auto_compaction_confidence_cap),
                signals.importance > self.auto_compaction_confidence_cap,
            )
        } else {
            (signals.importance, false)
        };

        Ok(CaptureDecision::Accept(CapturePlan {
            confidence: raw_confidence,
            superseded_ids: dedup_sorted(signals.suggested_supersession_ids),
            conflict_ids: dedup_sorted(signals.suggested_conflict_ids),
            confidence_capped,
        }))
    }
}

fn blend_actionability(signals: &CaptureSignals) -> f32 {
    ACTIONABILITY_IMPORTANCE_WEIGHT * signals.importance
        + ACTIONABILITY_IMPACT_WEIGHT * signals.impact
        + ACTIONABILITY_RARITY_WEIGHT * signals.rarity
}

fn validate_unit_finite(field: &str, value: f32) -> Result<()> {
    if !value.is_finite() {
        bail!("capture signal {field} must be finite");
    }
    ensure!(
        (0.0..=1.0).contains(&value),
        "capture signal {field} must be in [0, 1]"
    );
    Ok(())
}

fn dedup_sorted(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let trimmed = id.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.clone()) {
            out.push(trimmed);
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate() -> CaptureGate {
        CaptureGate::default()
    }

    fn safe_signals() -> CaptureSignals {
        CaptureSignals {
            significance: 0.7,
            importance: 0.6,
            impact: 0.5,
            rarity: 0.5,
            source_trust: SourceTrust::User,
            safety: CaptureSafety::Safe,
            novelty: NoveltyDisposition::Novel,
            has_valid_evidence: false,
            suggested_supersession_ids: Vec::new(),
            suggested_conflict_ids: Vec::new(),
        }
    }

    #[test]
    fn accepts_novel_safe_above_thresholds() {
        let decision = gate().evaluate(safe_signals()).expect("evaluate");
        match decision {
            CaptureDecision::Accept(plan) => {
                assert!((plan.confidence - 0.6).abs() < f32::EPSILON);
                assert!(!plan.confidence_capped);
                assert!(plan.superseded_ids.is_empty());
                assert!(plan.conflict_ids.is_empty());
            }
            other => panic!("expected accept, got {other:?}"),
        }
    }

    #[test]
    fn skips_low_significance_below_threshold() {
        let mut signals = safe_signals();
        signals.significance = 0.3;
        match gate().evaluate(signals).expect("evaluate") {
            CaptureDecision::Skip {
                reason,
                significance,
                ..
            } => {
                assert_eq!(reason, SkipReason::LowSignificance);
                assert!((significance - 0.3).abs() < f32::EPSILON);
            }
            other => panic!("expected skip, got {other:?}"),
        }
    }

    #[test]
    fn skips_low_actionability_when_blend_is_weak() {
        let mut signals = safe_signals();
        signals.significance = 0.6;
        signals.importance = 0.1;
        signals.impact = 0.1;
        signals.rarity = 0.1;
        // actionability = 0.4*0.1 + 0.3*0.1 + 0.3*0.1 = 0.1
        match gate().evaluate(signals).expect("evaluate") {
            CaptureDecision::Skip {
                reason,
                actionability,
                ..
            } => {
                assert_eq!(reason, SkipReason::LowActionability);
                assert!((actionability - 0.1).abs() < f32::EPSILON);
            }
            other => panic!("expected skip, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_always_skips_regardless_of_score() {
        let mut signals = safe_signals();
        signals.novelty = NoveltyDisposition::Duplicate;
        match gate().evaluate(signals).expect("evaluate") {
            CaptureDecision::Skip { reason, .. } => assert_eq!(reason, SkipReason::Duplicate),
            other => panic!("expected duplicate skip, got {other:?}"),
        }
    }

    #[test]
    fn quarantines_secret_leak_before_score_checks() {
        let mut signals = safe_signals();
        signals.safety = CaptureSafety::SecretLeak;
        match gate().evaluate(signals).expect("evaluate") {
            CaptureDecision::Quarantine { reason } => {
                assert_eq!(reason, QuarantineReason::SecretLeak);
            }
            other => panic!("expected quarantine, got {other:?}"),
        }
    }

    #[test]
    fn quarantines_injection_before_score_checks() {
        let mut signals = safe_signals();
        signals.safety = CaptureSafety::Injection;
        match gate().evaluate(signals).expect("evaluate") {
            CaptureDecision::Quarantine { reason } => {
                assert_eq!(reason, QuarantineReason::Injection);
            }
            other => panic!("expected quarantine, got {other:?}"),
        }
    }

    #[test]
    fn quarantines_untrusted_claim_without_evidence() {
        let mut signals = safe_signals();
        signals.source_trust = SourceTrust::Untrusted;
        signals.safety = CaptureSafety::UntrustedClaim;
        signals.has_valid_evidence = false;
        match gate().evaluate(signals).expect("evaluate") {
            CaptureDecision::Quarantine { reason } => {
                assert_eq!(reason, QuarantineReason::UntrustedClaimWithoutEvidence);
            }
            other => panic!("expected quarantine, got {other:?}"),
        }
    }

    #[test]
    fn accepts_untrusted_claim_with_valid_evidence_when_score_passes() {
        let mut signals = safe_signals();
        signals.source_trust = SourceTrust::Untrusted;
        signals.safety = CaptureSafety::UntrustedClaim;
        signals.has_valid_evidence = true;
        match gate().evaluate(signals).expect("evaluate") {
            CaptureDecision::Accept(plan) => {
                assert!((plan.confidence - 0.6).abs() < f32::EPSILON);
                assert!(!plan.confidence_capped);
            }
            other => panic!("expected accept, got {other:?}"),
        }
    }

    #[test]
    fn caps_untrusted_confidence_at_auto_compaction_limit() {
        let mut signals = safe_signals();
        signals.source_trust = SourceTrust::Untrusted;
        signals.importance = 0.9;
        match gate().evaluate(signals).expect("evaluate") {
            CaptureDecision::Accept(plan) => {
                assert!((plan.confidence - 0.6).abs() < f32::EPSILON);
                assert!(plan.confidence_capped);
            }
            other => panic!("expected accept, got {other:?}"),
        }
    }

    #[test]
    fn rejects_non_finite_signals() {
        let mut signals = safe_signals();
        signals.significance = f32::NAN;
        assert!(gate().evaluate(signals).is_err());

        let mut signals = safe_signals();
        signals.importance = f32::INFINITY;
        assert!(gate().evaluate(signals).is_err());
    }

    #[test]
    fn rejects_out_of_range_signals() {
        let mut signals = safe_signals();
        signals.significance = 1.5;
        assert!(gate().evaluate(signals).is_err());

        let mut signals = safe_signals();
        signals.impact = -0.1;
        assert!(gate().evaluate(signals).is_err());
    }

    #[test]
    fn supersession_and_conflict_ids_are_deduped_trimmed_and_sorted() {
        let mut signals = safe_signals();
        signals.suggested_supersession_ids = vec![
            "mem_b".to_string(),
            " mem_a ".to_string(),
            "mem_b".to_string(),
            String::new(),
        ];
        signals.suggested_conflict_ids = vec!["mem_z".to_string(), "mem_a".to_string()];
        match gate().evaluate(signals).expect("evaluate") {
            CaptureDecision::Accept(plan) => {
                assert_eq!(
                    plan.superseded_ids,
                    vec!["mem_a".to_string(), "mem_b".to_string()]
                );
                assert_eq!(
                    plan.conflict_ids,
                    vec!["mem_a".to_string(), "mem_z".to_string()]
                );
            }
            other => panic!("expected accept, got {other:?}"),
        }
    }

    #[test]
    fn rejects_oversized_relation_id_lists() {
        let mut signals = safe_signals();
        signals.suggested_supersession_ids =
            vec!["mem_x".to_string(); MAX_SUGGESTED_RELATION_IDS + 1];
        assert!(gate().evaluate(signals).is_err());
    }

    #[test]
    fn actionability_blend_matches_specified_weights() {
        let mut signals = safe_signals();
        signals.significance = 0.9;
        signals.importance = 0.4;
        signals.impact = 0.3;
        signals.rarity = 0.3;
        // blend = 0.4*0.4 + 0.3*0.3 + 0.3*0.3 = 0.34 -> below 0.5 threshold
        match gate().evaluate(signals).expect("evaluate") {
            CaptureDecision::Skip {
                reason,
                actionability,
                ..
            } => {
                assert_eq!(reason, SkipReason::LowActionability);
                assert!((actionability - 0.34).abs() < f32::EPSILON);
            }
            other => panic!("expected low-actionability skip, got {other:?}"),
        }
    }

    #[test]
    fn default_thresholds_match_phase_1_spec() {
        let gate = CaptureGate::default();
        assert!((gate.significance_threshold() - 0.5).abs() < f32::EPSILON);
        assert!((gate.actionability_threshold() - 0.5).abs() < f32::EPSILON);
        assert!((gate.auto_compaction_confidence_cap() - 0.6).abs() < f32::EPSILON);
    }
}
