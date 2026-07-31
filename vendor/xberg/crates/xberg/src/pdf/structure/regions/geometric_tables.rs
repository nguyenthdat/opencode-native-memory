//! Geometric table-region fallback (xberg-io/xberg#1316, #1319).
//!
//! Under the corrected square-resize RT-DETR preprocessing (commit `8299d3ea`,
//! faithful to the Docling Heron ONNX export), the layout detector no longer
//! emits a `Table` region for thin, sparse, borderless tables — an A4 page
//! squashed to 640×640 vertically compresses a 2–3 row grid below the model's
//! detection sensitivity. Reverting the preprocessing is not an option (it would
//! re-break the official-contract fix), so this module recovers those tables
//! from column-aligned text geometry instead.
//!
//! It synthesizes a `Table` region **only** for pages where the ML detector
//! produced no `Table` region at all — it never overrides the detector where it
//! already fired. The synthesized region is reconstructed through the same
//! guarded path as an ML `Table` hint
//! ([`super::tables::extract_tables_from_layout_hints`]), so the #36
//! false-positive guards (`post_process_table`, `is_well_formed_table`'s
//! empty-cell / shredded-row / alpha-ratio / unique-word checks, the
//! code-listing and single-cell-row guards) still decide acceptance. The one
//! exception is text-heavy grids (#1319): because a regular key-value grid trips
//! `is_well_formed_table`'s uniform-column *prose* heuristic, and this fallback
//! has already vetted the columnar structure geometrically ([`run_is_text_heavy_grid`]),
//! that single heuristic is skipped for these pre-vetted hints
//! (`prevalidated_columns`). All other guards still apply.

use crate::pdf::structure::types::{LayoutHint, LayoutHintClass};
use crate::pdf::table_reconstruct::HocrWord;

/// Minimum vertically-contiguous tabular rows to propose a table.
const MIN_TABLE_ROWS: usize = 3;
/// Minimum consistent column anchors a run must share to propose a table.
/// Three columns is the key precision guard: one- and two-column prose reflows
/// (references, two-column body text) cannot clear it, so they never reach the
/// downstream guards as table candidates.
const MIN_TABLE_COLS: usize = 3;
/// Two column left-edges within this many points are treated as the same column.
const ANCHOR_TOLERANCE_PTS: u32 = 10;
/// A column anchor counts as "consistent" when at least this fraction of the
/// run's rows place a segment at it.
const MIN_ANCHOR_ROW_SUPPORT: f32 = 0.6;
/// A run breaks when the top-to-top pitch to the next tabular row exceeds this
/// multiple of the median word height — keeps a genuine grid together while
/// refusing to bridge a table into distant prose.
const MAX_ROW_PITCH_FACTOR: f32 = 3.5;
/// Rows whose tops differ by at most this multiple of the median word height are
/// the same visual row.
const ROW_GROUPING_FACTOR: f32 = 0.6;
/// Minimum fraction of a run's words that must be numeric-value tokens for the
/// run to be proposed via the numeric path. The #1316 target class — borderless
/// invoice / line-item / metric tables — is numeric, whereas the dominant
/// geometric false positive is alphabetic multi-column prose (two-column
/// academic body text), which cannot clear this bar. Text-heavy borderless
/// tables (invoice header key-value grids, #1319) are handled by the separate,
/// strictly-gated [`run_is_text_heavy_grid`] path.
const MIN_NUMERIC_WORD_FRACTION: f32 = 0.35;
/// Text-heavy key-value grids (#1319 — invoice header association tables) are
/// numeric-sparse, so they fail [`MIN_NUMERIC_WORD_FRACTION`], but they carry a
/// columnar signature that the numeric gate's dominant false positive (packed
/// multi-column academic prose) does not: short cells separated by whitespace
/// gutters that are both far wider than the inter-word spacing and absolutely
/// wide. All three conditions below must hold. They were calibrated against a
/// corpus sweep of numeric-sparse runs: alphabetic prose peaks at ~77 pt gutters
/// only when dense (≥14 words/row), while sparse prose (≤10 words/row) never
/// exceeds ~12 pt gutters — versus the reproducer's 229 pt gutter at 7 words/row.
/// Each threshold independently rejects every observed prose run; requiring all
/// three is defence-in-depth against layouts outside the sample.
const TEXT_HEAVY_MAX_WORDS_PER_ROW: usize = 10;
/// A run's median inter-column gutter must be at least this multiple of its
/// median intra-cell word spacing — the gutter is a deliberate column break, not
/// wide word spacing.
const TEXT_HEAVY_MIN_GUTTER_RATIO: u32 = 8;
/// A run's median inter-column gutter must be at least this many points — an
/// absolute column gap (~1.4 in), well above the widest sparse-prose gutter.
const TEXT_HEAVY_MIN_GUTTER_PTS: u32 = 100;
/// Confidence stamped on a synthesized hint. `extract_tables_from_layout_hints`
/// filters hints by `confidence >= min_confidence` (0.5 at every call site), so
/// a synthesized region must clear that bar; the geometric evidence has already
/// been vetted by the row/column thresholds above.
const SYNTHETIC_HINT_CONFIDENCE: f32 = 1.0;

/// A visual row: word indices sharing a top band, plus the row's top anchor.
struct Row {
    word_indices: Vec<usize>,
    top: u32,
}

/// Detect column-aligned multi-row text bands and return them as synthetic
/// `Table` hints in PDF (bottom-origin) coordinates, matching the coordinate
/// contract of ML-produced [`LayoutHint`]s (`hint_img_top = page_height -
/// hint.top`). Returns an empty vector when no band clears the thresholds or the
/// `XBERG_LAYOUT_NO_GEOMETRIC_TABLES` toggle is set.
///
/// Words whose center falls inside one of `existing_table_hints` (ML `Table`
/// regions already claimed on this page) are dropped before grouping. This lets
/// the geometric fallback recover a *second*, spatially separate tabular region
/// on a page where the ML detector already found one table elsewhere (#1321) —
/// the page-wide "ML found a table, skip the fallback entirely" branch otherwise
/// starves that second region. Pass `&[]` to consider every word.
///
/// `existing_table_hints` are in PDF (bottom-origin) coordinates, matching the
/// ML `LayoutHint` contract; they are converted to image-top-origin (matching
/// `HocrWord`) via `(left, page_height - top, right, page_height - bottom)`
/// before the containment check, mirroring [`run_bounding_hint`]'s inverse
/// conversion.
pub(in crate::pdf::structure) fn detect_geometric_table_hints(
    words: &[HocrWord],
    page_height: f32,
    existing_table_hints: &[&LayoutHint],
) -> Vec<LayoutHint> {
    if crate::pdf::structure::layout_debug::layout_debug_flags().no_geometric_tables {
        return Vec::new();
    }

    let exclusion_boxes: Vec<(f32, f32, f32, f32)> = existing_table_hints
        .iter()
        .map(|h| (h.left, page_height - h.top, h.right, page_height - h.bottom))
        .collect();

    let indexed: Vec<usize> = words
        .iter()
        .enumerate()
        .filter(|(_, w)| !w.text.trim().is_empty())
        .filter(|(_, w)| {
            let cx = w.left as f32 + w.width as f32 / 2.0;
            let cy = w.top as f32 + w.height as f32 / 2.0;
            !exclusion_boxes
                .iter()
                .any(|&(l, t, r, b)| cx >= l && cx <= r && cy >= t && cy <= b)
        })
        .map(|(i, _)| i)
        .collect();
    if indexed.len() < MIN_TABLE_ROWS * MIN_TABLE_COLS {
        return Vec::new();
    }

    let median_height = median_word_height(words, &indexed);
    if median_height == 0 {
        return Vec::new();
    }

    let rows = group_rows(words, &indexed, median_height);
    if rows.len() < MIN_TABLE_ROWS {
        return Vec::new();
    }

    let max_row_pitch = (median_height as f32 * MAX_ROW_PITCH_FACTOR).round() as u32;

    // Split rows into vertically-contiguous runs (top-to-top pitch within
    // `max_row_pitch`), then propose each run whose columns are consistently
    // aligned. Column consistency — not per-row gap segmentation — is the real
    // signal: prose reflows share a left margin but no ≥3 internal columns.
    let mut hints = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    for row_idx in 0..rows.len() {
        let contiguous = run
            .last()
            .map(|&prev| rows[row_idx].top.saturating_sub(rows[prev].top) <= max_row_pitch)
            .unwrap_or(true);
        if !contiguous {
            if let Some(hint) = finalize_run(words, &rows, &run, page_height) {
                hints.push(hint);
            }
            run.clear();
        }
        run.push(row_idx);
    }
    if let Some(hint) = finalize_run(words, &rows, &run, page_height) {
        hints.push(hint);
    }

    hints
}

/// Evaluate a run of vertically-contiguous rows: emit a hint only when the run
/// has enough rows and enough word-left anchors are consistently aligned across
/// them (≥[`MIN_TABLE_COLS`] columns present in ≥[`MIN_ANCHOR_ROW_SUPPORT`] of
/// the rows).
fn finalize_run(words: &[HocrWord], rows: &[Row], run: &[usize], page_height: f32) -> Option<LayoutHint> {
    if run.len() < MIN_TABLE_ROWS {
        return None;
    }

    // Each word's left edge is a candidate column anchor. A cell of multiple
    // words contributes one anchor per word, but only anchors aligned across
    // enough rows survive the support filter, so stray second-word offsets that
    // appear in a single row (e.g. a two-word header cell) drop out.
    let mut anchors: Vec<u32> = Vec::new();
    let mut per_row_lefts: Vec<Vec<u32>> = Vec::with_capacity(run.len());
    for &row_idx in run {
        let mut lefts: Vec<u32> = rows[row_idx].word_indices.iter().map(|&i| words[i].left).collect();
        lefts.sort_unstable();
        anchors.extend(lefts.iter().copied());
        per_row_lefts.push(lefts);
    }
    anchors.sort_unstable();

    let min_support = ((run.len() as f32 * MIN_ANCHOR_ROW_SUPPORT).ceil() as usize).max(2);
    let consistent_columns = count_consistent_anchors(&anchors, &per_row_lefts, min_support);
    if consistent_columns < MIN_TABLE_COLS {
        return None;
    }

    // Accept the run if it is either numeric-dominant (the #1316 class) or a
    // text-heavy key-value grid (the #1319 class). Both are gated hard; a run
    // that is neither is left to the ML detector to avoid #36 over-fabrication.
    let numeric = run_is_numeric_dominant(words, rows, run);
    if !numeric && !run_is_text_heavy_grid(words, rows, run) {
        return None;
    }
    if !numeric {
        tracing::debug!(
            rows = run.len(),
            columns = consistent_columns,
            "geometric table fallback: accepted text-heavy key-value grid (#1319)"
        );
    }

    Some(run_bounding_hint(words, rows, run, page_height))
}

/// Whether a run's words are numeric-dominant — the separator between the
/// numeric borderless tables this fallback targets (#1316) and the alphabetic
/// multi-column prose that is its dominant false positive.
fn run_is_numeric_dominant(words: &[HocrWord], rows: &[Row], run: &[usize]) -> bool {
    let mut total = 0usize;
    let mut numeric = 0usize;
    for &row_idx in run {
        for &i in &rows[row_idx].word_indices {
            let text = words[i].text.trim();
            if text.is_empty() {
                continue;
            }
            total += 1;
            if is_numeric_token(text) {
                numeric += 1;
            }
        }
    }
    total > 0 && numeric as f32 >= total as f32 * MIN_NUMERIC_WORD_FRACTION
}

/// A token reads as a numeric data value when it carries at least one digit and
/// no alphabetic character — a real number, optionally decorated with grouping,
/// decimal, sign, currency, or percent punctuation. Catches `10.00`, `19%`,
/// `1,234`, `$305,568`, `2024-01`, `(42,253)`; rejects prose words, unit labels
/// like `000s`, and math variables/subscripts like `a1`, `2j`, `γδ` — the latter
/// being the dominant false positive on equation-dense academic pages (#1316).
fn is_numeric_token(text: &str) -> bool {
    let mut has_digit = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            has_digit = true;
        } else if c.is_alphabetic() {
            return false;
        }
    }
    has_digit
}

/// Cluster anchors within [`ANCHOR_TOLERANCE_PTS`] and count clusters that are
/// populated in at least `min_support` distinct rows.
fn count_consistent_anchors(sorted_anchors: &[u32], per_row_starts: &[Vec<u32>], min_support: usize) -> usize {
    let mut cluster_centers: Vec<u32> = Vec::new();
    let mut cluster_start = 0usize;
    while cluster_start < sorted_anchors.len() {
        let base = sorted_anchors[cluster_start];
        let mut cluster_end = cluster_start + 1;
        while cluster_end < sorted_anchors.len()
            && sorted_anchors[cluster_end].saturating_sub(base) <= ANCHOR_TOLERANCE_PTS
        {
            cluster_end += 1;
        }
        let center = sorted_anchors[cluster_start..cluster_end]
            .iter()
            .map(|&v| v as u64)
            .sum::<u64>()
            / (cluster_end - cluster_start) as u64;
        cluster_centers.push(center as u32);
        cluster_start = cluster_end;
    }

    cluster_centers
        .iter()
        .filter(|&&center| {
            let supporting_rows = per_row_starts
                .iter()
                .filter(|starts| starts.iter().any(|&s| s.abs_diff(center) <= ANCHOR_TOLERANCE_PTS))
                .count();
            supporting_rows >= min_support
        })
        .count()
}

/// Bounding hint over every word in the run's rows, in PDF (bottom-origin)
/// coordinates so it matches the ML-hint contract consumed by
/// `extract_tables_from_layout_hints`.
fn run_bounding_hint(words: &[HocrWord], rows: &[Row], run: &[usize], page_height: f32) -> LayoutHint {
    let mut min_left = u32::MAX;
    let mut max_right = 0u32;
    let mut min_top = u32::MAX;
    let mut max_bottom = 0u32;
    for &row_idx in run {
        for &i in &rows[row_idx].word_indices {
            let w = &words[i];
            min_left = min_left.min(w.left);
            max_right = max_right.max(w.left + w.width);
            min_top = min_top.min(w.top);
            max_bottom = max_bottom.max(w.top + w.height);
        }
    }

    LayoutHint {
        class_name: LayoutHintClass::Table,
        confidence: SYNTHETIC_HINT_CONFIDENCE,
        left: min_left as f32,
        right: max_right as f32,
        top: page_height - min_top as f32,
        bottom: page_height - max_bottom as f32,
    }
}

/// Group filtered words into visual rows by top coordinate. `indexed` holds the
/// non-empty word indices; the returned rows preserve those indices.
fn group_rows(words: &[HocrWord], indexed: &[usize], median_height: u32) -> Vec<Row> {
    let mut order: Vec<usize> = indexed.to_vec();
    order.sort_by_key(|&i| (words[i].top, words[i].left));

    let tolerance = ((median_height as f32 * ROW_GROUPING_FACTOR).round() as u32).max(2);
    let mut rows: Vec<Row> = Vec::new();
    for &i in &order {
        match rows.last_mut() {
            Some(row) if words[i].top.saturating_sub(row.top) <= tolerance => {
                row.word_indices.push(i);
            }
            _ => rows.push(Row {
                word_indices: vec![i],
                top: words[i].top,
            }),
        }
    }
    rows
}

/// Whether a run is a text-heavy key-value grid (#1319): sparse rows whose cells
/// are separated by whitespace gutters both far wider than the intra-cell word
/// spacing and absolutely wide. See [`TEXT_HEAVY_MAX_WORDS_PER_ROW`] for the
/// calibration. This is the non-numeric counterpart to [`run_is_numeric_dominant`].
fn run_is_text_heavy_grid(words: &[HocrWord], rows: &[Row], run: &[usize]) -> bool {
    let (median_gutter, median_word_gap, median_words_per_row) = run_row_spacing(words, rows, run);
    median_word_gap > 0
        && median_words_per_row <= TEXT_HEAVY_MAX_WORDS_PER_ROW
        && median_gutter >= TEXT_HEAVY_MIN_GUTTER_PTS
        && median_gutter >= median_word_gap.saturating_mul(TEXT_HEAVY_MIN_GUTTER_RATIO)
}

/// Per-run spacing summary used by [`run_is_text_heavy_grid`]:
/// `(median inter-column gutter, median intra-cell word gap, median words/row)`.
/// For each row the widest inter-word gap approximates the column gutter and the
/// narrowest positive gap approximates the word spacing inside a cell; medians
/// over the run resist a single irregular row.
fn run_row_spacing(words: &[HocrWord], rows: &[Row], run: &[usize]) -> (u32, u32, usize) {
    let mut per_row_max_gap: Vec<u32> = Vec::new();
    let mut per_row_min_gap: Vec<u32> = Vec::new();
    let mut per_row_count: Vec<usize> = Vec::new();
    for &row_idx in run {
        let mut row_words: Vec<&HocrWord> = rows[row_idx].word_indices.iter().map(|&i| &words[i]).collect();
        row_words.sort_by_key(|w| w.left);
        per_row_count.push(row_words.len());
        let gaps: Vec<u32> = row_words
            .windows(2)
            .map(|pair| pair[1].left.saturating_sub(pair[0].left + pair[0].width))
            .collect();
        if let Some(&max_gap) = gaps.iter().max() {
            per_row_max_gap.push(max_gap);
        }
        if let Some(&min_gap) = gaps.iter().filter(|&&g| g > 0).min() {
            per_row_min_gap.push(min_gap);
        }
    }
    (
        median_u32(per_row_max_gap),
        median_u32(per_row_min_gap),
        median_usize(per_row_count),
    )
}

fn median_u32(mut values: Vec<u32>) -> u32 {
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or(0)
}

fn median_usize(mut values: Vec<usize>) -> usize {
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or(0)
}

fn median_word_height(words: &[HocrWord], indexed: &[usize]) -> u32 {
    let mut heights: Vec<u32> = indexed.iter().map(|&i| words[i].height).collect();
    heights.sort_unstable();
    heights.get(heights.len() / 2).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, left: u32, top: u32, width: u32) -> HocrWord {
        HocrWord {
            text: text.to_string(),
            left,
            top,
            width,
            height: 10,
            confidence: 95.0,
        }
    }

    /// The #1316 reproducer shape: a 5-column, header + 2-data-row borderless
    /// numeric grid. The detector must propose one Table region spanning it.
    #[test]
    fn detects_sparse_five_column_borderless_grid() {
        let xs = [40u32, 320, 400, 470, 540];
        let headers = ["Description", "Quantity", "Unit", "VAT", "Total"];
        let mut words = Vec::new();
        for (x, h) in xs.iter().zip(headers) {
            words.push(word(h, *x, 100, 60));
        }
        for (row, vals) in [
            ["PROD_1", "1", "10.00", "19%", "10.00"],
            ["PROD_2", "2", "20.00", "19%", "40.00"],
        ]
        .iter()
        .enumerate()
        {
            let y = 120 + row as u32 * 15;
            for (x, v) in xs.iter().zip(vals) {
                words.push(word(v, *x, y, 40));
            }
        }

        let hints = detect_geometric_table_hints(&words, 842.0, &[]);
        assert_eq!(hints.len(), 1, "expected exactly one synthesized table region");
        let hint = &hints[0];
        assert_eq!(hint.class_name, LayoutHintClass::Table);
        // Region must be in PDF coords with top above bottom and cover the grid.
        assert!(hint.top > hint.bottom, "PDF top must exceed bottom");
        assert!(hint.left <= 40.0 && hint.right >= 580.0, "region must span the columns");
    }

    /// Two-column prose reflow (references, body text) has only two column
    /// anchors and must NOT be proposed — the ≥3-column guard rejects it before
    /// it can reach the downstream reconstruction guards.
    #[test]
    fn rejects_two_column_prose() {
        let mut words = Vec::new();
        for row in 0..5u32 {
            let y = 100 + row * 14;
            words.push(word("some", 40, y, 80));
            words.push(word("phrase", 300, y, 90));
        }
        let hints = detect_geometric_table_hints(&words, 842.0, &[]);
        assert!(hints.is_empty(), "2-column prose must not be proposed as a table");
    }

    /// An alphabetic 3-column grid (e.g. a word/name list) is NOT proposed:
    /// the numeric-dominance gate reserves this fallback for numeric borderless
    /// tables and leaves textual tables to the ML detector (#1316 scope).
    #[test]
    fn rejects_alphabetic_three_column_grid() {
        let xs = [40u32, 200, 360];
        let mut words = Vec::new();
        for row in 0..4u32 {
            let y = 100 + row * 15;
            for (c, x) in xs.iter().enumerate() {
                words.push(word(&format!("word{row}{c}"), *x, y, 70));
            }
        }
        let hints = detect_geometric_table_hints(&words, 842.0, &[]);
        assert!(hints.is_empty(), "alphabetic grid must not be proposed (numeric gate)");
    }

    /// The #1319 reproducer shape: a sparse 3-column key-value header grid with
    /// short multi-word cells separated by wide gutters. It is numeric-sparse, so
    /// it clears detection only through the text-heavy path.
    #[test]
    fn detects_text_heavy_key_value_grid() {
        let col_a = 33u32; // recipient / address
        let col_b = 330u32; // field label
        let col_c = 460u32; // value
        let rows_text = [
            ("SAMPLE", "ROAD", "Invoice", "number", "INV", "alpha"),
            ("DEMO", "CITY", "Order", "number", "ORD", "beta"),
            ("SYNTH", "COUNTRY", "Invoice", "date", "Jan", "gamma"),
            ("EXAMPLE", "CORP", "Order", "date", "Feb", "delta"),
        ];
        let mut words = Vec::new();
        for (r, (a1, a2, b1, b2, c1, c2)) in rows_text.iter().enumerate() {
            let y = 100 + r as u32 * 15;
            words.push(word(a1, col_a, y, 40)); // ends 73
            words.push(word(a2, col_a + 45, y, 30)); // 78 → gutter 222 to col_b
            words.push(word(b1, col_b, y, 48)); // ends 378
            words.push(word(b2, col_b + 53, y, 42)); // 383
            words.push(word(c1, col_c, y, 28)); // ends 488
            words.push(word(c2, col_c + 33, y, 28)); // 493
        }
        let hints = detect_geometric_table_hints(&words, 842.0, &[]);
        assert_eq!(hints.len(), 1, "text-heavy key-value grid must be proposed");
        assert_eq!(hints[0].class_name, LayoutHintClass::Table);
    }

    /// Packed multi-column academic prose: many words per row, gutters comparable
    /// to the inter-word spacing. Numeric-sparse and not a sparse key-value grid,
    /// so neither acceptance path fires.
    #[test]
    fn rejects_dense_multicolumn_prose() {
        let col_x = [33u32, 220, 410];
        let mut words = Vec::new();
        for r in 0..5u32 {
            let y = 100 + r * 14;
            for &cx in &col_x {
                let mut x = cx;
                for w in 0..5u32 {
                    words.push(word(&format!("word{r}{w}"), x, y, 30));
                    x += 34; // 30 width + 4 gap → packed cell
                }
            }
        }
        let hints = detect_geometric_table_hints(&words, 842.0, &[]);
        assert!(hints.is_empty(), "dense multi-column prose must not be proposed");
    }

    /// The dangerous corpus false positive: a dense prose row (many words) that
    /// happens to carry one wide gap, giving a high gutter ratio. The words-per-row
    /// sparsity cap rejects it even though the gutter and ratio guards pass.
    #[test]
    fn rejects_wide_gutter_dense_prose() {
        let col_x = [33u32, 250, 470];
        let mut words = Vec::new();
        for r in 0..4u32 {
            let y = 100 + r * 14;
            for &cx in &col_x {
                let mut x = cx;
                for w in 0..6u32 {
                    words.push(word(&format!("w{r}{w}"), x, y, 8));
                    x += 12; // packed: 12 words visible per row across 3 dense cells
                }
            }
        }
        let hints = detect_geometric_table_hints(&words, 842.0, &[]);
        assert!(
            hints.is_empty(),
            "wide-gutter but dense prose must be rejected by the sparsity cap"
        );
    }

    /// An equation-dense grid whose "numbers" are math variables and subscripts
    /// (`a1`, `2j`, `j0`, `γδ`) must NOT be proposed: `is_numeric_token` rejects
    /// any letter-bearing token, so the run fails the numeric-dominance gate.
    /// Regression guard for the 1304.6413 academic-paper false positive (#1316).
    #[test]
    fn rejects_equation_subscript_grid() {
        let xs = [40u32, 200, 360, 520];
        let cells = [
            ["a1", "2j", "j0", "γδ"],
            ["b1", "2b", "t1", "wn"],
            ["x1", "y1", "2a", "nx2"],
        ];
        let mut words = Vec::new();
        for (row, vals) in cells.iter().enumerate() {
            let y = 100 + row as u32 * 15;
            for (x, v) in xs.iter().zip(vals) {
                words.push(word(v, *x, y, 40));
            }
        }
        let hints = detect_geometric_table_hints(&words, 842.0, &[]);
        assert!(
            hints.is_empty(),
            "equation-subscript grid must not be proposed (numeric gate)"
        );
    }

    /// A single tabular row (no vertical repetition) is not a table.
    #[test]
    fn rejects_single_tabular_row() {
        let words = vec![
            word("A", 40, 100, 30),
            word("B", 200, 100, 30),
            word("C", 400, 100, 30),
            word("prose", 40, 200, 300),
        ];
        let hints = detect_geometric_table_hints(&words, 842.0, &[]);
        assert!(hints.is_empty(), "a lone tabular row must not be proposed");
    }

    /// The `XBERG_LAYOUT_NO_GEOMETRIC_TABLES` A/B toggle short-circuits detection.
    /// (Env-driven; asserted indirectly via the empty-input fast path staying
    /// empty — the flag is exercised by the benchmark A/B, not unit state.)
    #[test]
    fn empty_input_returns_no_hints() {
        assert!(detect_geometric_table_hints(&[], 842.0, &[]).is_empty());
    }

    /// #1321 reproducer shape: a page with an upper borderless key-value grid
    /// (the #1319 text-heavy shape) AND a lower, spatially separate cluster
    /// already claimed by an ML `Table` hint. The page-wide `has_table_hint`
    /// gate in `pipeline.rs` must no longer suppress geometric recovery of the
    /// upper region just because the ML detector found a table elsewhere.
    #[test]
    fn detects_text_heavy_grid_when_page_also_has_ml_table_hint() {
        const PAGE_HEIGHT: f32 = 842.0;
        let col_a = 33u32;
        let col_b = 330u32;
        let col_c = 460u32;
        let rows_text = [
            ("SAMPLE", "ROAD", "Invoice", "number", "INV", "alpha"),
            ("DEMO", "CITY", "Order", "number", "ORD", "beta"),
            ("SYNTH", "COUNTRY", "Invoice", "date", "Jan", "gamma"),
            ("EXAMPLE", "CORP", "Order", "date", "Feb", "delta"),
        ];
        let mut words = Vec::new();
        for (r, (a1, a2, b1, b2, c1, c2)) in rows_text.iter().enumerate() {
            let y = 100 + r as u32 * 15;
            words.push(word(a1, col_a, y, 40));
            words.push(word(a2, col_a + 45, y, 30));
            words.push(word(b1, col_b, y, 48));
            words.push(word(b2, col_b + 53, y, 42));
            words.push(word(c1, col_c, y, 28));
            words.push(word(c2, col_c + 33, y, 28));
        }

        // Spatially separate lower cluster, already recognized by the ML
        // detector as a `Table` region — its shape doesn't matter, only that
        // it is excluded before the row-grouping body runs.
        let lower_xs = [40u32, 200, 360];
        let lower_min_top = 400u32;
        let mut lower_max_bottom = 0u32;
        let mut lower_max_right = 0u32;
        for r in 0..3u32 {
            let y = lower_min_top + r * 15;
            for &x in &lower_xs {
                let w = word("line", x, y, 50);
                lower_max_bottom = lower_max_bottom.max(w.top + w.height);
                lower_max_right = lower_max_right.max(w.left + w.width);
                words.push(w);
            }
        }
        let lower_hint = LayoutHint {
            class_name: LayoutHintClass::Table,
            confidence: 1.0,
            left: lower_xs[0] as f32,
            right: lower_max_right as f32,
            top: PAGE_HEIGHT - lower_min_top as f32,
            bottom: PAGE_HEIGHT - lower_max_bottom as f32,
        };

        let hints = detect_geometric_table_hints(&words, PAGE_HEIGHT, &[&lower_hint]);
        assert_eq!(
            hints.len(),
            1,
            "expected exactly one synthesized region, bounding only the upper grid"
        );
        let hint = &hints[0];
        assert_eq!(hint.class_name, LayoutHintClass::Table);
        // The synthesized region must not extend down into the excluded lower
        // cluster's image-top band (bottom, in PDF coords, stays above it).
        let synthesized_bottom_image_top = PAGE_HEIGHT - hint.bottom;
        assert!(
            synthesized_bottom_image_top < lower_min_top as f32,
            "synthesized region must not reach into the excluded lower cluster"
        );
    }

    /// A single grid, fully covered by an `existing_table_hints` bbox, must be
    /// excluded entirely — every word's center falls inside the exclusion box,
    /// so no rows survive to be grouped.
    #[test]
    fn excludes_words_inside_existing_table_hint() {
        const PAGE_HEIGHT: f32 = 842.0;
        let xs = [40u32, 320, 400, 470, 540];
        let headers = ["Description", "Quantity", "Unit", "VAT", "Total"];
        let mut words = Vec::new();
        for (x, h) in xs.iter().zip(headers) {
            words.push(word(h, *x, 100, 60));
        }
        for (row, vals) in [
            ["PROD_1", "1", "10.00", "19%", "10.00"],
            ["PROD_2", "2", "20.00", "19%", "40.00"],
        ]
        .iter()
        .enumerate()
        {
            let y = 120 + row as u32 * 15;
            for (x, v) in xs.iter().zip(vals) {
                words.push(word(v, *x, y, 40));
            }
        }

        let covering_hint = LayoutHint {
            class_name: LayoutHintClass::Table,
            confidence: 1.0,
            left: 0.0,
            right: 700.0,
            top: PAGE_HEIGHT,
            bottom: 0.0,
        };

        let hints = detect_geometric_table_hints(&words, PAGE_HEIGHT, &[&covering_hint]);
        assert!(
            hints.is_empty(),
            "a grid fully covered by an existing hint must be excluded"
        );
    }
}
