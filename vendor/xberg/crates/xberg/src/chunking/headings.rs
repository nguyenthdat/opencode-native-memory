//! Heading extraction for Markdown chunk metadata.
//!
//! Parses markdown text to build a heading map, then resolves
//! which headings a chunk falls under based on its byte offset.

use crate::types::{HeadingContext, HeadingLevel, PageBoundary};
use pulldown_cmark::{Event, Options, Parser, TagEnd};

/// An entry in the heading map: `(byte_offset, level, text)`.
type HeadingEntry = (usize, u8, String);

/// Build a heading map from markdown text.
///
/// Returns a sorted Vec of `(byte_offset, level, heading_text)` for each heading found.
pub(crate) fn build_heading_map(markdown: &str) -> Vec<HeadingEntry> {
    let parser = Parser::new_ext(markdown, Options::all());
    let mut headings = Vec::new();
    let mut current_heading: Option<(usize, u8)> = None;
    let mut heading_text = String::new();

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(pulldown_cmark::Tag::Heading { level, .. }) => {
                current_heading = Some((range.start, heading_level_to_u8(level)));
                heading_text.clear();
            }
            Event::Text(text) if current_heading.is_some() => {
                heading_text.push_str(&text);
            }
            Event::Code(code) if current_heading.is_some() => {
                heading_text.push_str(&code);
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((offset, level)) = current_heading.take() {
                    headings.push((offset, level, heading_text.clone()));
                }
            }
            _ => {}
        }
    }
    headings
}

/// Resolve the heading context for a chunk at the given byte offset.
///
/// Walks the heading map to find all headings that precede `byte_start`,
/// building a proper hierarchy stack (h1 > h2 > h3, etc.).
///
/// When `page_boundaries` is provided, heading context is scoped to the page the
/// chunk starts on: headings declared on an *earlier* page are excluded from the
/// stack (#1289). Without this, a heading detected once (e.g. a document title on
/// page 1) would keep leaking into every later chunk for the rest of the combined
/// text — including onto pages belonging to an entirely different, concatenated
/// document that happens to carry no heading of its own. Headings declared on the
/// same page as the chunk still build the usual nested hierarchy. When
/// `page_boundaries` is `None` or empty (or the offset falls in no boundary), the
/// heading map is walked unrestricted, matching the pre-#1289 behavior.
pub(crate) fn resolve_heading_context(
    byte_start: usize,
    heading_map: &[HeadingEntry],
    page_boundaries: Option<&[PageBoundary]>,
) -> Option<HeadingContext> {
    let page_start = page_boundaries.and_then(|boundaries| page_start_for_offset(byte_start, boundaries));

    let mut stack: Vec<HeadingLevel> = Vec::new();

    for &(offset, level, ref text) in heading_map {
        if offset > byte_start {
            break;
        }
        if page_start.is_some_and(|page_start| offset < page_start) {
            continue;
        }
        while stack.last().is_some_and(|h| h.level >= level) {
            stack.pop();
        }
        stack.push(HeadingLevel {
            level,
            text: text.clone(),
        });
    }

    if stack.is_empty() {
        None
    } else {
        Some(HeadingContext { headings: stack })
    }
}

/// Return the byte offset where the page containing `byte_start` begins.
///
/// Falls back to the closest preceding boundary's start when `byte_start` doesn't
/// fall strictly within any `[byte_start, byte_end)` range (e.g. a zero-width
/// boundary for a blank page, or an offset landing exactly at content end), so a
/// page can still be resolved for boundary sets with such edge-case entries.
/// Returns `None` if no boundary starts at or before `byte_start`.
fn page_start_for_offset(byte_start: usize, page_boundaries: &[PageBoundary]) -> Option<usize> {
    page_boundaries
        .iter()
        .find(|b| byte_start >= b.byte_start && byte_start < b.byte_end)
        .or_else(|| page_boundaries.iter().rev().find(|b| b.byte_start <= byte_start))
        .map(|b| b.byte_start)
}

fn heading_level_to_u8(level: pulldown_cmark::HeadingLevel) -> u8 {
    match level {
        pulldown_cmark::HeadingLevel::H1 => 1,
        pulldown_cmark::HeadingLevel::H2 => 2,
        pulldown_cmark::HeadingLevel::H3 => 3,
        pulldown_cmark::HeadingLevel::H4 => 4,
        pulldown_cmark::HeadingLevel::H5 => 5,
        pulldown_cmark::HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_heading_map_basic() {
        let md = "# Title\n\nSome text.\n\n## Section 1\n\nContent.\n\n## Section 2\n\nMore content.";
        let map = build_heading_map(md);
        assert_eq!(map.len(), 3);
        assert_eq!(map[0], (0, 1, "Title".to_string()));
        assert_eq!(map[1].1, 2);
        assert_eq!(map[1].2, "Section 1");
        assert_eq!(map[2].1, 2);
        assert_eq!(map[2].2, "Section 2");
    }

    #[test]
    fn test_build_heading_map_nested() {
        let md = "# H1\n\n## H2\n\n### H3\n\nText.";
        let map = build_heading_map(md);
        assert_eq!(map.len(), 3);
        assert_eq!(map[0].1, 1);
        assert_eq!(map[1].1, 2);
        assert_eq!(map[2].1, 3);
    }

    #[test]
    fn test_build_heading_map_no_headings() {
        let md = "Just plain text without any headings.";
        let map = build_heading_map(md);
        assert!(map.is_empty());
    }

    #[test]
    fn test_build_heading_map_with_code_in_heading() {
        let md = "# Title with `code`\n\nText.";
        let map = build_heading_map(md);
        assert_eq!(map.len(), 1);
        assert_eq!(map[0].2, "Title with code");
    }

    #[test]
    fn test_resolve_heading_context_under_h2() {
        let map = vec![
            (0, 1, "Title".to_string()),
            (10, 2, "Section A".to_string()),
            (30, 2, "Section B".to_string()),
        ];
        let ctx = resolve_heading_context(15, &map, None).unwrap();
        assert_eq!(ctx.headings.len(), 2);
        assert_eq!(ctx.headings[0].level, 1);
        assert_eq!(ctx.headings[0].text, "Title");
        assert_eq!(ctx.headings[1].level, 2);
        assert_eq!(ctx.headings[1].text, "Section A");
    }

    #[test]
    fn test_resolve_heading_context_root() {
        let map = vec![(10, 1, "Title".to_string())];
        let ctx = resolve_heading_context(0, &map, None);
        assert!(ctx.is_none());
    }

    #[test]
    fn test_resolve_heading_context_superseded() {
        let map = vec![
            (0, 1, "Title".to_string()),
            (10, 2, "Section A".to_string()),
            (20, 3, "Subsection".to_string()),
            (30, 2, "Section B".to_string()),
        ];
        let ctx = resolve_heading_context(35, &map, None).unwrap();
        assert_eq!(ctx.headings.len(), 2);
        assert_eq!(ctx.headings[1].text, "Section B");
    }

    #[test]
    fn test_resolve_heading_context_deep_nesting() {
        let map = vec![
            (0, 1, "H1".to_string()),
            (5, 2, "H2".to_string()),
            (10, 3, "H3".to_string()),
            (15, 4, "H4".to_string()),
        ];
        let ctx = resolve_heading_context(20, &map, None).unwrap();
        assert_eq!(ctx.headings.len(), 4);
        assert_eq!(ctx.headings[3].level, 4);
    }

    /// Regression test for #1289: a heading declared on an earlier page must not
    /// leak into a later page's chunk when that later page carries no heading of
    /// its own — the classic symptom for concatenated documents (e.g. doc 1's
    /// title showing up on doc 2's / doc 3's un-headed pages).
    #[test]
    fn test_resolve_heading_context_resets_at_page_boundary_without_new_heading() {
        let map = vec![(0, 1, "Doc 1 Title".to_string())];
        let page_boundaries = [
            PageBoundary {
                page_number: 1,
                byte_start: 0,
                byte_end: 20,
            },
            PageBoundary {
                page_number: 2,
                byte_start: 20,
                byte_end: 40,
            },
            PageBoundary {
                page_number: 3,
                byte_start: 40,
                byte_end: 60,
            },
        ];

        let page_1_ctx = resolve_heading_context(5, &map, Some(&page_boundaries));
        assert_eq!(
            page_1_ctx.unwrap().headings[0].text,
            "Doc 1 Title",
            "the chunk on the page the heading was declared on must still see it"
        );

        let page_2_ctx = resolve_heading_context(25, &map, Some(&page_boundaries));
        assert!(
            page_2_ctx.is_none(),
            "page 2 has no heading of its own and must NOT inherit doc 1's title, got: {:?}",
            page_2_ctx
        );

        let page_3_ctx = resolve_heading_context(45, &map, Some(&page_boundaries));
        assert!(
            page_3_ctx.is_none(),
            "page 3 has no heading of its own and must NOT inherit doc 1's title, got: {:?}",
            page_3_ctx
        );
    }

    /// A heading declared on the same page as the chunk (even mid-page, after
    /// other content) must still build the normal nested hierarchy.
    #[test]
    fn test_resolve_heading_context_same_page_hierarchy_still_builds() {
        let map = vec![(20, 1, "Section".to_string()), (25, 2, "Subsection".to_string())];
        let page_boundaries = [PageBoundary {
            page_number: 2,
            byte_start: 20,
            byte_end: 40,
        }];

        let ctx = resolve_heading_context(30, &map, Some(&page_boundaries)).unwrap();
        assert_eq!(ctx.headings.len(), 2);
        assert_eq!(ctx.headings[0].text, "Section");
        assert_eq!(ctx.headings[1].text, "Subsection");
    }

    /// Without page boundary information, behavior is unchanged from pre-#1289:
    /// headings apply for the rest of the text regardless of any (absent) page
    /// structure.
    #[test]
    fn test_resolve_heading_context_no_page_boundaries_is_unrestricted() {
        let map = vec![(0, 1, "Title".to_string())];
        let ctx = resolve_heading_context(1000, &map, None);
        assert_eq!(ctx.unwrap().headings[0].text, "Title");
    }
}
