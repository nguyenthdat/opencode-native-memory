//! Diagnostic toggles for the ML layout override sites.
//!
//! Every flag defaults **off** (its environment variable unset), so production
//! behavior is unchanged. The toggles exist to attribute the per-site SF1 impact
//! of the layout path: disabling one override at a time and re-running the
//! head-to-head benchmark isolates how much each site currently helps or hurts.
//!
//! | Env var                        | Effect when set                                              |
//! |--------------------------------|-------------------------------------------------------------|
//! | `XBERG_LAYOUT_NO_DEMOTE`       | Never demote a heading to body text on Text/Caption/Footnote |
//! | `XBERG_LAYOUT_NO_PROMOTE`      | Never promote a paragraph to a heading on Title/SectionHeader |
//! | `XBERG_LAYOUT_NO_LAYOUT_TABLES`| Never fabricate a table from a layout Table region           |
//! | `XBERG_LAYOUT_NO_GEOMETRIC_TABLES`| Never synthesize a Table region from column-aligned text   |
//! | `XBERG_LAYOUT_NO_REORDER`      | Never reorder reading order by layout regions                |
//! | `XBERG_LAYOUT_LOG_OVERRIDES`   | Emit a per-override `info` line for attribution              |
//!
//! A value is "set" when the variable is present and not empty, `0`, or `false`.

use std::sync::OnceLock;

/// Diagnostic toggles, read once from the environment.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LayoutDebugFlags {
    /// `XBERG_LAYOUT_NO_DEMOTE`: suppress heading→body demotion.
    pub(crate) no_demote: bool,
    /// `XBERG_LAYOUT_NO_PROMOTE`: suppress paragraph→heading promotion.
    pub(crate) no_promote: bool,
    /// `XBERG_LAYOUT_NO_LAYOUT_TABLES`: suppress table fabrication from layout regions.
    pub(crate) no_layout_tables: bool,
    /// `XBERG_LAYOUT_NO_GEOMETRIC_TABLES`: suppress synthesizing Table regions from
    /// column-aligned text on pages where the ML detector emitted no Table region.
    /// Exists to A/B the geometric fallback (xberg-io/xberg#1316) against the corpus.
    pub(crate) no_geometric_tables: bool,
    /// `XBERG_LAYOUT_NO_REORDER`: suppress reading-order reordering by layout.
    /// Its only reader lives in `reorder_segments_by_layout`, which is gated on
    /// `feature = "layout-detection"`; without that feature (wasm-target, android-target,
    /// Swift's feature set) the field is never read, so allow dead_code there.
    #[cfg_attr(not(feature = "layout-detection"), allow(dead_code))]
    pub(crate) no_reorder: bool,
    /// `XBERG_LAYOUT_LOG_OVERRIDES`: emit a per-override attribution line.
    pub(crate) log_overrides: bool,
}

fn env_is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| {
        let value = value.to_string_lossy();
        let trimmed = value.trim();
        !trimmed.is_empty() && trimmed != "0" && !trimmed.eq_ignore_ascii_case("false")
    })
}

/// Return the process-wide layout diagnostic flags, reading the environment once.
pub(crate) fn layout_debug_flags() -> LayoutDebugFlags {
    static FLAGS: OnceLock<LayoutDebugFlags> = OnceLock::new();
    *FLAGS.get_or_init(|| LayoutDebugFlags {
        no_demote: env_is_set("XBERG_LAYOUT_NO_DEMOTE"),
        no_promote: env_is_set("XBERG_LAYOUT_NO_PROMOTE"),
        no_layout_tables: env_is_set("XBERG_LAYOUT_NO_LAYOUT_TABLES"),
        no_geometric_tables: env_is_set("XBERG_LAYOUT_NO_GEOMETRIC_TABLES"),
        no_reorder: env_is_set("XBERG_LAYOUT_NO_REORDER"),
        log_overrides: env_is_set("XBERG_LAYOUT_LOG_OVERRIDES"),
    })
}
