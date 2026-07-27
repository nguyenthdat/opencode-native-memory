//! Bounded local document extraction and deterministic Markdown chunking.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use xberg::{ExtractInput, ExtractionConfig, OutputFormat, SecurityLimits};

use crate::MemoryConfig;
use crate::config::hash_hex;
use crate::validation::MAX_CONTENT_CHARS;

const MAX_DOCUMENT_PATH_CHARS: usize = 200;
pub(crate) const MAX_DOCUMENT_FILE_BYTES: u64 = 32 * 1_024 * 1_024;
const MAX_EXTRACTED_CHARS: usize = 600_000;
const MAX_DOCUMENT_CHUNKS: usize = 100;

pub(crate) struct ExtractedDocument {
    pub(crate) path: String,
    pub(crate) mime_type: String,
    pub(crate) content_hash: String,
    pub(crate) content: String,
    pub(crate) warnings: Vec<String>,
}

pub(crate) struct InspectedDocument {
    path: PathBuf,
    pub(crate) normalized_path: String,
    pub(crate) mime_type: String,
    pub(crate) content_hash: String,
    bytes: Vec<u8>,
}

pub(crate) fn inspect_document(
    config: &MemoryConfig,
    requested_path: &str,
) -> Result<InspectedDocument> {
    let (path, normalized_path) = validated_document_path(config, requested_path)?;
    let bytes =
        fs::read(&path).with_context(|| format!("cannot read document `{normalized_path}`"))?;
    let content_hash = hash_hex(&bytes);
    let mime_type = mime_type_for_path(&path)?;
    Ok(InspectedDocument {
        path,
        normalized_path,
        mime_type,
        content_hash,
        bytes,
    })
}

pub(crate) fn extract_inspected_document(
    inspected: InspectedDocument,
) -> Result<ExtractedDocument> {
    let filename = inspected
        .path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("document filename must be valid UTF-8"))?
        .to_string();
    let input =
        ExtractInput::from_bytes(inspected.bytes, inspected.mime_type.clone(), Some(filename));
    let limits = SecurityLimits {
        max_content_size: MAX_EXTRACTED_CHARS * 4,
        ..SecurityLimits::default()
    };
    let extraction = ExtractionConfig {
        use_cache: false,
        enable_quality_processing: false,
        disable_ocr: true,
        extraction_timeout_secs: None,
        security_limits: Some(limits),
        output_format: OutputFormat::Markdown,
        escape_markdown: false,
        ..ExtractionConfig::default()
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .context("cannot initialize xberg extraction runtime")?;
    let mut output = runtime
        .block_on(xberg::extract(input, &extraction))
        .with_context(|| format!("cannot extract document `{}`", inspected.normalized_path))?;
    if !output.errors.is_empty() {
        let message = output
            .errors
            .iter()
            .take(3)
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        bail!(
            "xberg could not extract `{}`: {message}",
            inspected.normalized_path
        );
    }
    ensure!(
        output.results.len() == 1,
        "xberg returned {} documents for one input `{}`",
        output.results.len(),
        inspected.normalized_path
    );
    let document = output.results.pop().ok_or_else(|| {
        anyhow::anyhow!(
            "xberg returned no content for `{}`",
            inspected.normalized_path
        )
    })?;
    let content = document.content.trim().to_string();
    ensure!(
        !content.is_empty(),
        "document `{}` contains no extractable text",
        inspected.normalized_path
    );
    ensure!(
        content.chars().count() <= MAX_EXTRACTED_CHARS,
        "extracted document exceeds {MAX_EXTRACTED_CHARS} characters"
    );
    let warnings = document
        .processing_warnings
        .into_iter()
        .take(20)
        .map(|warning| format!("{}: {}", warning.source, warning.message))
        .collect();
    Ok(ExtractedDocument {
        path: inspected.normalized_path,
        mime_type: document.mime_type.into_owned(),
        content_hash: inspected.content_hash,
        content,
        warnings,
    })
}

pub(crate) fn is_supported_document_path(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "pdf" | "md" | "markdown" | "html" | "htm"
    )
}

pub(crate) fn split_markdown(content: &str) -> Result<Vec<String>> {
    let mut blocks = Vec::new();
    let mut block = String::new();
    let mut in_fence = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        if line.trim().is_empty() && !in_fence {
            if !block.trim().is_empty() {
                blocks.push(block.trim().to_string());
                block.clear();
            }
            continue;
        }
        if !block.is_empty() {
            block.push('\n');
        }
        block.push_str(line);
    }
    if !block.trim().is_empty() {
        blocks.push(block.trim().to_string());
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    for block in blocks {
        if block.chars().count() > MAX_CONTENT_CHARS {
            flush_chunk(&mut current, &mut chunks);
            for part in hard_split(&block, MAX_CONTENT_CHARS) {
                chunks.push(part);
            }
            continue;
        }
        let separator = usize::from(!current.is_empty()) * 2;
        if current.chars().count() + separator + block.chars().count() > MAX_CONTENT_CHARS {
            flush_chunk(&mut current, &mut chunks);
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(&block);
    }
    flush_chunk(&mut current, &mut chunks);
    ensure!(
        !chunks.is_empty(),
        "document contains no extractable text chunks"
    );
    ensure!(
        chunks.len() <= MAX_DOCUMENT_CHUNKS,
        "document requires {} chunks; maximum is {MAX_DOCUMENT_CHUNKS}",
        chunks.len()
    );
    Ok(chunks)
}

fn validated_document_path(
    config: &MemoryConfig,
    requested_path: &str,
) -> Result<(std::path::PathBuf, String)> {
    let trimmed = requested_path.trim();
    ensure!(!trimmed.is_empty(), "document path cannot be empty");
    ensure!(
        trimmed.chars().count() <= MAX_DOCUMENT_PATH_CHARS,
        "document path exceeds {MAX_DOCUMENT_PATH_CHARS} characters"
    );
    ensure!(
        !trimmed.contains('\0'),
        "document path cannot contain NUL bytes"
    );
    let relative = Path::new(trimmed);
    ensure!(
        !relative.is_absolute(),
        "document path must be relative to the project root"
    );
    ensure!(
        relative
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "document path cannot contain parent, root, or platform-prefix components"
    );
    let root = config
        .project_root()
        .canonicalize()
        .context("cannot resolve project root for document ingestion")?;
    let joined = root.join(relative);
    let metadata = fs::symlink_metadata(&joined).with_context(|| {
        format!(
            "document `{trimmed}` does not exist under `{}`",
            root.display()
        )
    })?;
    ensure!(
        !metadata.file_type().is_symlink(),
        "document path cannot be a symlink"
    );
    ensure!(
        metadata.is_file(),
        "document path must reference a regular file"
    );
    ensure!(
        metadata.len() <= MAX_DOCUMENT_FILE_BYTES,
        "document exceeds {} MiB",
        MAX_DOCUMENT_FILE_BYTES / 1_024 / 1_024
    );
    let canonical = joined
        .canonicalize()
        .with_context(|| format!("cannot resolve document `{trimmed}`"))?;
    let relative = canonical
        .strip_prefix(&root)
        .context("document resolves outside the project root")?;
    let normalized = relative.to_string_lossy().replace('\\', "/");
    Ok((canonical, normalized))
}

fn mime_type_for_path(path: &Path) -> Result<String> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "pdf" => Ok("application/pdf".to_string()),
        "md" | "markdown" => Ok("text/markdown".to_string()),
        "html" | "htm" => Ok("text/html".to_string()),
        _ => bail!("unsupported document format; expected .pdf, .md, .markdown, .html, or .htm"),
    }
}

fn flush_chunk(current: &mut String, chunks: &mut Vec<String>) {
    if !current.is_empty() {
        chunks.push(std::mem::take(current));
    }
}

fn hard_split(value: &str, max_chars: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for word in value.split_whitespace() {
        if word.chars().count() > max_chars {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            let chars = word.chars().collect::<Vec<_>>();
            for chunk in chars.chunks(max_chars) {
                parts.push(chunk.iter().collect());
            }
            continue;
        }
        let separator = usize::from(!current.is_empty());
        if current.chars().count() + separator + word.chars().count() > max_chars {
            parts.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::{MAX_CONTENT_CHARS, hard_split, split_markdown};
    use crate::MemoryConfig;

    #[test]
    fn markdown_chunks_are_bounded_and_deterministic() {
        let content = format!("# Heading\n\n{}\n\nTail", "word ".repeat(2_000));
        let first = split_markdown(&content).expect("content should split");
        let second = split_markdown(&content).expect("content should split deterministically");
        assert_eq!(first, second);
        assert!(first.len() > 1);
        assert!(
            first
                .iter()
                .all(|chunk| chunk.chars().count() <= MAX_CONTENT_CHARS)
        );
    }

    #[test]
    fn hard_split_preserves_unicode_without_exceeding_limit() {
        let parts = hard_split(&"đ".repeat(25), 10);
        assert_eq!(
            parts
                .iter()
                .map(|part| part.chars().count())
                .collect::<Vec<_>>(),
            [10, 10, 5]
        );
    }

    #[test]
    fn fenced_blocks_ignore_blank_paragraph_boundaries() {
        let chunks = split_markdown("```rust\nfn main() {\n\n}\n```\n\nAfter")
            .expect("fenced markdown should split");
        assert_eq!(chunks, ["```rust\nfn main() {\n\n}\n```\n\nAfter"]);
    }

    #[test]
    fn extracts_markdown_through_xberg() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let document = directory.path().join("paper.md");
        std::fs::write(&document, "# Paper\n\nA durable finding.")
            .expect("document should be written");
        let config = MemoryConfig::new(
            directory.path().to_path_buf(),
            directory.path().join("data"),
            directory.path().join("models"),
        );

        let inspected = super::inspect_document(&config, "paper.md").expect("inspect Markdown");
        let extracted =
            super::extract_inspected_document(inspected).expect("xberg should extract Markdown");

        assert_eq!(extracted.path, "paper.md");
        assert!(extracted.content.contains("durable finding"));
        assert!(!extracted.content_hash.is_empty());
    }
}
