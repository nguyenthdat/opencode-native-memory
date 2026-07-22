//! Phase 1 memory taxonomy, family grouping, and retrieval profiles.
//!
//! The taxonomy is a deterministic, type-aware classification layer that sits
//! on top of the existing `MemoryKind`/`MemoryScope`/code-anchor signals. It
//! drives retrieval profile selection in Phase 1 and is stored on every v3
//! lifecycle record. Inference is a pure fallback used when a caller does not
//! supply an explicit taxonomy; it never overrides an explicit value.

use serde::{Deserialize, Serialize};

use crate::contract::{CodeAnchor, MemoryKind, MemoryScope};

/// The 15-variant Phase 1 memory taxonomy.
///
/// Variants are serialised as `snake_case` strings and are stable for the v3
/// state schema. The `Default` variant is `SessionSummary` to match the
/// `MemoryKind::Summary` default so that `MemoryTaxonomy::default()` agrees
/// with `MemoryTaxonomy::infer(MemoryKind::default(), MemoryScope::default(),
/// &[])`.
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTaxonomy {
    TaskAttempt,
    ToolCall,
    #[default]
    SessionSummary,
    ArchitectureFact,
    CodebaseFact,
    UserFact,
    FixPattern,
    CodeTemplate,
    ToolHeuristic,
    CodeStyle,
    LibraryPref,
    WorkflowPref,
    Decision,
    TeamConvention,
    ProjectStandard,
}

impl MemoryTaxonomy {
    /// Stable wire string for the variant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskAttempt => "task_attempt",
            Self::ToolCall => "tool_call",
            Self::SessionSummary => "session_summary",
            Self::ArchitectureFact => "architecture_fact",
            Self::CodebaseFact => "codebase_fact",
            Self::UserFact => "user_fact",
            Self::FixPattern => "fix_pattern",
            Self::CodeTemplate => "code_template",
            Self::ToolHeuristic => "tool_heuristic",
            Self::CodeStyle => "code_style",
            Self::LibraryPref => "library_pref",
            Self::WorkflowPref => "workflow_pref",
            Self::Decision => "decision",
            Self::TeamConvention => "team_convention",
            Self::ProjectStandard => "project_standard",
        }
    }

    /// Parse a wire string into a taxonomy variant.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown string.
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "task_attempt" => Ok(Self::TaskAttempt),
            "tool_call" => Ok(Self::ToolCall),
            "session_summary" => Ok(Self::SessionSummary),
            "architecture_fact" => Ok(Self::ArchitectureFact),
            "codebase_fact" => Ok(Self::CodebaseFact),
            "user_fact" => Ok(Self::UserFact),
            "fix_pattern" => Ok(Self::FixPattern),
            "code_template" => Ok(Self::CodeTemplate),
            "tool_heuristic" => Ok(Self::ToolHeuristic),
            "code_style" => Ok(Self::CodeStyle),
            "library_pref" => Ok(Self::LibraryPref),
            "workflow_pref" => Ok(Self::WorkflowPref),
            "decision" => Ok(Self::Decision),
            "team_convention" => Ok(Self::TeamConvention),
            "project_standard" => Ok(Self::ProjectStandard),
            _ => anyhow::bail!("unknown memory taxonomy: {value}"),
        }
    }

    /// The high-level family this taxonomy belongs to.
    #[must_use]
    pub const fn family(self) -> MemoryFamily {
        match self {
            Self::TaskAttempt | Self::ToolCall | Self::SessionSummary => MemoryFamily::Episodic,
            Self::ArchitectureFact | Self::CodebaseFact | Self::UserFact => MemoryFamily::Semantic,
            Self::FixPattern | Self::CodeTemplate | Self::ToolHeuristic => MemoryFamily::Procedural,
            Self::CodeStyle | Self::LibraryPref | Self::WorkflowPref => MemoryFamily::Preference,
            Self::Decision | Self::ProjectStandard => MemoryFamily::Decision,
            Self::TeamConvention => MemoryFamily::Team,
        }
    }

    /// The retrieval profile used to score memories of this taxonomy.
    #[must_use]
    pub const fn retrieval_profile(self) -> RetrievalProfile {
        self.family().retrieval_profile()
    }

    /// Deterministically infer a taxonomy from legacy lifecycle signals.
    ///
    /// The inference is a pure function of `MemoryKind`, `MemoryScope`, and the
    /// presence of code anchors. It is used only when a caller does not supply
    /// an explicit taxonomy. The rules, in priority order, are:
    ///
    /// 1. Repository `Decision`/`Fact` -> `ProjectStandard`.
    /// 2. Other repository kinds -> `TeamConvention`.
    /// 3. `Decision` -> `Decision`.
    /// 4. `Preference` -> `WorkflowPref`.
    /// 5. Anchored `Fact` -> `CodebaseFact`, otherwise `ArchitectureFact`.
    /// 6. Anchored `Pattern` -> `CodeTemplate`.
    /// 7. Unanchored `Pattern`/`Gotcha` -> `FixPattern`.
    /// 8. `Summary` -> `SessionSummary`.
    #[must_use]
    pub fn infer(kind: MemoryKind, scope: MemoryScope, code_anchors: &[CodeAnchor]) -> Self {
        Self::infer_anchored(kind, scope, !code_anchors.is_empty())
    }

    /// Same as [`infer`](Self::infer) but takes a boolean anchor flag directly,
    /// for use before code anchors have been captured.
    #[must_use]
    pub fn infer_anchored(kind: MemoryKind, scope: MemoryScope, anchored: bool) -> Self {
        match (scope, kind) {
            (MemoryScope::Repository, MemoryKind::Decision | MemoryKind::Fact) => {
                Self::ProjectStandard
            }
            (MemoryScope::Repository, _) => Self::TeamConvention,
            (_, MemoryKind::Decision) => Self::Decision,
            (_, MemoryKind::Preference) => Self::WorkflowPref,
            (_, MemoryKind::Fact) if anchored => Self::CodebaseFact,
            (_, MemoryKind::Fact) => Self::ArchitectureFact,
            (_, MemoryKind::Pattern) if anchored => Self::CodeTemplate,
            (_, MemoryKind::Pattern | MemoryKind::Gotcha) => Self::FixPattern,
            (_, MemoryKind::Summary) => Self::SessionSummary,
        }
    }
}

/// High-level family grouping for retrieval profile selection.
///
/// `Decision` and `Team` families share the `Semantic` retrieval profile's
/// weight blend, as specified by the Phase 1 design, but are kept as distinct
/// families for future profile divergence and diagnostics.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum MemoryFamily {
    Semantic,
    Decision,
    Team,
    Procedural,
    Preference,
    Episodic,
}

impl MemoryFamily {
    /// The retrieval profile used for this family.
    #[must_use]
    pub const fn retrieval_profile(self) -> RetrievalProfile {
        match self {
            Self::Semantic | Self::Decision | Self::Team => RetrievalProfile::Semantic,
            Self::Procedural => RetrievalProfile::Procedural,
            Self::Preference => RetrievalProfile::Preference,
            Self::Episodic => RetrievalProfile::Episodic,
        }
    }
}

/// Retrieval weight blends applied per candidate during scoring.
///
/// Weights are `(dense, reciprocal_rank, lexical, channel_agreement)` and sum
/// to 1.0. `Episodic` uses the same base blend as `Semantic`; its shorter
/// retention behaviour comes from the existing `retention_factor` applied to
/// episodic taxonomies (which inherit short half-lives from `MemoryKind::Summary`).
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum RetrievalProfile {
    Semantic,
    Procedural,
    Preference,
    Episodic,
}

impl RetrievalProfile {
    /// `(dense, reciprocal_rank, lexical, channel_agreement)` weight blend.
    #[must_use]
    pub const fn weights(self) -> (f32, f32, f32, f32) {
        match self {
            Self::Semantic | Self::Episodic => (0.45, 0.25, 0.20, 0.10),
            Self::Procedural => (0.35, 0.25, 0.30, 0.10),
            Self::Preference => (0.25, 0.20, 0.45, 0.10),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MemoryFamily, MemoryTaxonomy, RetrievalProfile};
    use crate::contract::{CodeAnchor, MemoryKind, MemoryScope};

    fn anchor() -> CodeAnchor {
        CodeAnchor {
            path: "src/lib.rs".to_string(),
            sha256: "abc".to_string(),
            git_sha: None,
        }
    }

    #[test]
    fn serde_round_trips_all_variants() {
        for variant in [
            MemoryTaxonomy::TaskAttempt,
            MemoryTaxonomy::ToolCall,
            MemoryTaxonomy::SessionSummary,
            MemoryTaxonomy::ArchitectureFact,
            MemoryTaxonomy::CodebaseFact,
            MemoryTaxonomy::UserFact,
            MemoryTaxonomy::FixPattern,
            MemoryTaxonomy::CodeTemplate,
            MemoryTaxonomy::ToolHeuristic,
            MemoryTaxonomy::CodeStyle,
            MemoryTaxonomy::LibraryPref,
            MemoryTaxonomy::WorkflowPref,
            MemoryTaxonomy::Decision,
            MemoryTaxonomy::TeamConvention,
            MemoryTaxonomy::ProjectStandard,
        ] {
            let wire = serde_json::to_string(&variant).expect("serialize taxonomy");
            let parsed: MemoryTaxonomy = serde_json::from_str(&wire).expect("deserialize taxonomy");
            assert_eq!(variant, parsed);
            assert_eq!(variant.as_str(), wire.trim_matches('"'));
            assert_eq!(MemoryTaxonomy::parse(variant.as_str()).unwrap(), variant);
        }
    }

    #[test]
    fn unknown_taxonomy_string_is_rejected() {
        assert!(MemoryTaxonomy::parse("not_a_taxonomy").is_err());
    }

    #[test]
    fn family_and_profile_mapping_is_exhaustive_and_stable() {
        assert_eq!(MemoryTaxonomy::TaskAttempt.family(), MemoryFamily::Episodic);
        assert_eq!(
            MemoryTaxonomy::ArchitectureFact.family(),
            MemoryFamily::Semantic
        );
        assert_eq!(
            MemoryTaxonomy::FixPattern.family(),
            MemoryFamily::Procedural
        );
        assert_eq!(MemoryTaxonomy::CodeStyle.family(), MemoryFamily::Preference);
        assert_eq!(MemoryTaxonomy::Decision.family(), MemoryFamily::Decision);
        assert_eq!(
            MemoryTaxonomy::ProjectStandard.family(),
            MemoryFamily::Decision
        );
        assert_eq!(MemoryTaxonomy::TeamConvention.family(), MemoryFamily::Team);

        assert_eq!(
            MemoryTaxonomy::Decision.retrieval_profile(),
            RetrievalProfile::Semantic
        );
        assert_eq!(
            MemoryTaxonomy::TeamConvention.retrieval_profile(),
            RetrievalProfile::Semantic
        );
        assert_eq!(
            MemoryTaxonomy::FixPattern.retrieval_profile(),
            RetrievalProfile::Procedural
        );
        assert_eq!(
            MemoryTaxonomy::CodeStyle.retrieval_profile(),
            RetrievalProfile::Preference
        );
        assert_eq!(
            MemoryTaxonomy::TaskAttempt.retrieval_profile(),
            RetrievalProfile::Episodic
        );
    }

    #[test]
    fn profile_weights_sum_to_one() {
        for profile in [
            RetrievalProfile::Semantic,
            RetrievalProfile::Procedural,
            RetrievalProfile::Preference,
            RetrievalProfile::Episodic,
        ] {
            let (d, r, l, a) = profile.weights();
            let sum = d + r + l + a;
            assert!(
                (sum - 1.0).abs() < f32::EPSILON,
                "profile {profile:?} weights sum to {sum}"
            );
        }
        let (d, _r, l, _a) = RetrievalProfile::Procedural.weights();
        assert!(d < 0.45 && l > 0.20);
        let (d, _r, l, _a) = RetrievalProfile::Preference.weights();
        assert!(d < 0.45 && l > 0.20);
    }

    #[test]
    fn infer_handles_repository_scope_first() {
        assert_eq!(
            MemoryTaxonomy::infer(MemoryKind::Decision, MemoryScope::Repository, &[]),
            MemoryTaxonomy::ProjectStandard
        );
        assert_eq!(
            MemoryTaxonomy::infer(MemoryKind::Fact, MemoryScope::Repository, &[anchor()]),
            MemoryTaxonomy::ProjectStandard
        );
        assert_eq!(
            MemoryTaxonomy::infer(MemoryKind::Preference, MemoryScope::Repository, &[]),
            MemoryTaxonomy::TeamConvention
        );
        assert_eq!(
            MemoryTaxonomy::infer(MemoryKind::Pattern, MemoryScope::Repository, &[anchor()]),
            MemoryTaxonomy::TeamConvention
        );
    }

    #[test]
    fn infer_non_repository_kinds() {
        assert_eq!(
            MemoryTaxonomy::infer(MemoryKind::Decision, MemoryScope::Project, &[]),
            MemoryTaxonomy::Decision
        );
        assert_eq!(
            MemoryTaxonomy::infer(MemoryKind::Preference, MemoryScope::Agent, &[]),
            MemoryTaxonomy::WorkflowPref
        );
        assert_eq!(
            MemoryTaxonomy::infer(MemoryKind::Fact, MemoryScope::Project, &[anchor()]),
            MemoryTaxonomy::CodebaseFact
        );
        assert_eq!(
            MemoryTaxonomy::infer(MemoryKind::Fact, MemoryScope::Project, &[]),
            MemoryTaxonomy::ArchitectureFact
        );
        assert_eq!(
            MemoryTaxonomy::infer(MemoryKind::Pattern, MemoryScope::Project, &[anchor()]),
            MemoryTaxonomy::CodeTemplate
        );
        assert_eq!(
            MemoryTaxonomy::infer(MemoryKind::Pattern, MemoryScope::Project, &[]),
            MemoryTaxonomy::FixPattern
        );
        assert_eq!(
            MemoryTaxonomy::infer(MemoryKind::Gotcha, MemoryScope::Project, &[]),
            MemoryTaxonomy::FixPattern
        );
        assert_eq!(
            MemoryTaxonomy::infer(MemoryKind::Summary, MemoryScope::Project, &[]),
            MemoryTaxonomy::SessionSummary
        );
    }
}
