//! Source code extractor using tree-sitter language pack.
//!
//! Extracts content and structural analysis from source code files using
//! tree-sitter parsers. Language detection is performed via file extension
//! or shebang line.

use std::borrow::Cow;
use std::path::Path;

use async_trait::async_trait;
use tree_sitter_language_pack as tslp;

use crate::Result;
use crate::core::config::{CodeContentMode, ExtractionConfig};
use crate::core::mime::SOURCE_CODE_MIME_TYPE;
use crate::extractors::SyncExtractor;
use crate::internal_builder::InternalDocumentBuilder;
use crate::plugins::InternalDocumentExtractor;
use crate::plugins::Plugin;
use crate::types::internal::InternalDocument;
use crate::types::metadata::{
    CodeChunkInfo, CodeDataAttribute, CodeDataNode, CodeDataNodeKind, CodeMetadata, FormatMetadata, Metadata,
};
#[cfg_attr(alef, alef(skip))]
/// Source code extractor using tree-sitter language pack.
///
/// Detects the programming language from the file extension or shebang line,
/// then uses tree-sitter to parse and extract structural information.
pub struct CodeExtractor;

impl Default for CodeExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeExtractor {
    pub(crate) fn new() -> Self {
        Self
    }

    /// Build a `tslp::ProcessConfig` from the xberg `TreeSitterProcessConfig`.
    fn build_process_config(language: &str, config: &ExtractionConfig) -> tslp::ProcessConfig {
        if let Some(ref ts_config) = config.tree_sitter {
            let pc: tslp::ProcessConfig = (&ts_config.process).into();
            return tslp::ProcessConfig {
                language: Cow::Owned(language.to_string()),
                ..pc
            };
        }
        tslp::ProcessConfig::new(language)
    }

    /// Build a document that emits the raw source verbatim, with no tree-sitter
    /// processing. Used when tree-sitter is disabled via config.
    fn build_raw_document(source: &str, language: &str) -> InternalDocument {
        let mut builder = InternalDocumentBuilder::new("code");
        builder.push_code(source, Some(language), None, None);

        let mut doc = builder.build();
        doc.metadata = Metadata {
            format: Some(FormatMetadata::Code(CodeMetadata::default())),
            ..Default::default()
        };
        doc.mime_type = SOURCE_CODE_MIME_TYPE.to_string();
        doc
    }

    /// Heading level for a chunk's context marker: 2 for class/module-shaped
    /// containers, 3 for everything else (functions, methods, etc.).
    fn chunk_heading_level(chunk: &tslp::CodeChunk) -> u8 {
        if chunk.metadata.node_types.iter().any(|t| {
            matches!(
                t.as_str(),
                "class_definition" | "module_definition" | "class_declaration" | "module"
            )
        }) {
            2
        } else {
            3
        }
    }

    /// Extract from source text with a known language.
    fn extract_with_language(source: &str, language: &str, config: &ExtractionConfig) -> Result<InternalDocument> {
        let ts_config = config.tree_sitter.as_ref();

        if !ts_config.map(|c| c.enabled).unwrap_or(true) {
            return Ok(Self::build_raw_document(source, language));
        }

        let process_config = Self::build_process_config(language, config);
        let content_mode = ts_config.map(|c| c.process.content_mode).unwrap_or_default();

        let result = tslp::process(source, &process_config).map_err(|e| crate::XbergError::Parsing {
            message: format!("tree-sitter processing failed for language '{language}': {e}"),
            source: None,
        })?;

        let mut builder = InternalDocumentBuilder::new("code");
        let mut code_chunks: Vec<CodeChunkInfo> = Vec::with_capacity(result.chunks.len());

        if result.chunks.is_empty() {
            builder.push_code(source, Some(language), None, None);
        } else {
            for chunk in &result.chunks {
                match content_mode {
                    CodeContentMode::Raw => {}
                    CodeContentMode::Structure => {
                        if let Some(last_context) = chunk.metadata.context_path.last() {
                            let level = Self::chunk_heading_level(chunk);
                            builder.push_heading(level, last_context, None, None);
                        }
                    }
                    _ => {
                        if let Some(last_context) = chunk.metadata.context_path.last() {
                            let level = Self::chunk_heading_level(chunk);
                            builder.push_heading(level, last_context, None, None);
                        }
                        builder.push_code(&chunk.content, Some(language), None, None);
                    }
                }

                code_chunks.push(CodeChunkInfo {
                    text: chunk.content.clone(),
                    context_path: chunk.metadata.context_path.clone(),
                    node_types: chunk.metadata.node_types.clone(),
                    byte_start: chunk.start_byte,
                    byte_end: chunk.end_byte,
                });
            }

            if matches!(content_mode, CodeContentMode::Raw) {
                builder.push_code(source, Some(language), None, None);
            }
        }

        let mut doc = builder.build();
        doc.metadata = Metadata {
            format: Some(FormatMetadata::Code(CodeMetadata {
                chunks: code_chunks,
                data: result.data.as_ref().map(convert_data_node),
            })),
            ..Default::default()
        };
        doc.mime_type = SOURCE_CODE_MIME_TYPE.to_string();

        Ok(doc)
    }

    /// Detect language and read source from a file path.
    ///
    /// Returns `(language, source)`. Reads the file at most once.
    fn read_and_detect(path: &Path) -> Result<(String, String)> {
        let path_str = path.to_string_lossy();

        if let Some(lang) = tslp::detect_language_from_path(&path_str) {
            let source = std::fs::read_to_string(path)?;
            return Ok((lang.to_string(), source));
        }

        let source = std::fs::read_to_string(path)?;
        if let Some(lang) = tslp::detect_language_from_content(&source) {
            return Ok((lang.to_string(), source));
        }

        Err(crate::XbergError::UnsupportedFormat(format!(
            "Cannot detect programming language for: {}",
            path.display()
        )))
    }
}

impl Plugin for CodeExtractor {
    fn name(&self) -> &str {
        "code-extractor"
    }

    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    fn initialize(&self) -> Result<()> {
        Ok(())
    }

    fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    fn description(&self) -> &str {
        "Extracts content and structure from source code files using tree-sitter"
    }

    fn author(&self) -> &str {
        "Xberg Team"
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl InternalDocumentExtractor for CodeExtractor {
    async fn extract_content(
        &self,
        content: &[u8],
        _mime_type: &str,
        config: &ExtractionConfig,
    ) -> Result<InternalDocument> {
        tracing::debug!(format = "code", size_bytes = content.len(), "extraction starting");
        let source = String::from_utf8_lossy(content);

        let language = tslp::detect_language_from_content(&source)
            .or_else(|| config.source_name.as_deref().and_then(tslp::detect_language_from_path))
            .ok_or_else(|| {
                crate::XbergError::UnsupportedFormat(
                    "Cannot detect programming language from content (no shebang line). \
                     Use extract_file with a file path for extension-based detection."
                        .to_string(),
                )
            })?;

        let doc = Self::extract_with_language(&source, language, config)?;
        tracing::debug!(
            element_count = doc.elements.len(),
            format = "code",
            "extraction complete"
        );
        Ok(doc)
    }

    async fn extract_path(&self, path: &Path, _mime_type: &str, config: &ExtractionConfig) -> Result<InternalDocument> {
        let (language, source) = Self::read_and_detect(path)?;
        Self::extract_with_language(&source, &language, config)
    }

    fn supported_mime_types(&self) -> &[&str] {
        &[SOURCE_CODE_MIME_TYPE]
    }

    fn priority(&self) -> i32 {
        50
    }
}

impl SyncExtractor for CodeExtractor {
    fn extract_sync(&self, content: &[u8], _mime_type: &str, config: &ExtractionConfig) -> Result<InternalDocument> {
        let source = String::from_utf8_lossy(content);

        let language = tslp::detect_language_from_content(&source)
            .or_else(|| config.source_name.as_deref().and_then(tslp::detect_language_from_path))
            .ok_or_else(|| {
                crate::XbergError::UnsupportedFormat("Cannot detect programming language from content".to_string())
            })?;

        Self::extract_with_language(&source, language, config)
    }
}

/// Recursively map a `tree_sitter_language_pack::DataNode` to xberg's
/// FFI/binding-friendly [`CodeDataNode`], flattening `Span` down to byte offsets.
fn convert_data_node(node: &tslp::DataNode) -> CodeDataNode {
    CodeDataNode {
        kind: match node.kind {
            tslp::DataNodeKind::KeyValue => CodeDataNodeKind::KeyValue,
            tslp::DataNodeKind::Element => CodeDataNodeKind::Element,
            tslp::DataNodeKind::Sequence => CodeDataNodeKind::Sequence,
        },
        key: node.key.clone(),
        value: node.value.clone(),
        attributes: node
            .attributes
            .iter()
            .map(|attr| CodeDataAttribute {
                name: attr.name.clone(),
                value: attr.value.clone(),
                byte_start: attr.span.start_byte,
                byte_end: attr.span.end_byte,
            })
            .collect(),
        children: node.children.iter().map(convert_data_node).collect(),
        byte_start: node.span.start_byte,
        byte_end: node.span.end_byte,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Disabled tree-sitter config must skip TSLP processing entirely and emit the
    /// raw source as a single code element — this path must not call
    /// `tslp::process`, which needs grammar downloads at runtime.
    #[test]
    fn test_disabled_tree_sitter_emits_raw_source() {
        let config = ExtractionConfig {
            tree_sitter: Some(crate::core::config::TreeSitterConfig {
                enabled: false,
                ..Default::default()
            }),
            ..Default::default()
        };

        let source = "fn main() {\n    println!(\"hi\");\n}\n";
        let doc = CodeExtractor::extract_with_language(source, "rust", &config).expect("raw extraction must succeed");

        assert_eq!(doc.elements.len(), 1, "exactly one raw code element expected");
        assert_eq!(doc.mime_type, SOURCE_CODE_MIME_TYPE);

        let Some(FormatMetadata::Code(CodeMetadata { chunks, data })) = doc.metadata.format.as_ref() else {
            panic!("expected Code format metadata");
        };
        assert!(chunks.is_empty(), "raw path must not populate chunks");
        assert!(data.is_none(), "raw path must not populate data");
    }

    /// `TreeSitterProcessConfig::data_extraction` must map through to TSLP's
    /// `ProcessConfig::data_extraction` unchanged.
    #[test]
    fn test_process_config_maps_data_extraction() {
        let xberg_process_config = crate::core::config::TreeSitterProcessConfig {
            data_extraction: true,
            ..Default::default()
        };

        let tslp_process_config: tslp::ProcessConfig = (&xberg_process_config).into();

        assert!(tslp_process_config.data_extraction);
    }

    /// `convert_data_node` must map kind, key, value, attributes, children, and
    /// byte offsets from a hand-built `tslp::DataNode` tree.
    #[test]
    fn test_convert_data_node_maps_tree() {
        let child_span = tslp::Span {
            start_byte: 2,
            end_byte: 10,
            start_line: 0,
            start_column: 2,
            end_line: 0,
            end_column: 10,
        };
        let attr_span = tslp::Span {
            start_byte: 3,
            end_byte: 9,
            start_line: 0,
            start_column: 3,
            end_line: 0,
            end_column: 9,
        };
        let root_span = tslp::Span {
            start_byte: 0,
            end_byte: 12,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 12,
        };

        let child = tslp::DataNode {
            kind: tslp::DataNodeKind::Element,
            key: Some("host".to_string()),
            value: Some("localhost".to_string()),
            attributes: vec![tslp::DataAttribute {
                name: "class".to_string(),
                value: "primary".to_string(),
                span: attr_span,
            }],
            children: Vec::new(),
            span: child_span,
        };

        let root = tslp::DataNode {
            kind: tslp::DataNodeKind::KeyValue,
            key: None,
            value: None,
            attributes: Vec::new(),
            children: vec![child],
            span: root_span,
        };

        let converted = convert_data_node(&root);

        assert_eq!(converted.kind, CodeDataNodeKind::KeyValue);
        assert_eq!(converted.key, None);
        assert_eq!(converted.value, None);
        assert!(converted.attributes.is_empty());
        assert_eq!(converted.byte_start, 0);
        assert_eq!(converted.byte_end, 12);

        assert_eq!(converted.children.len(), 1);
        let converted_child = &converted.children[0];
        assert_eq!(converted_child.kind, CodeDataNodeKind::Element);
        assert_eq!(converted_child.key.as_deref(), Some("host"));
        assert_eq!(converted_child.value.as_deref(), Some("localhost"));
        assert_eq!(converted_child.byte_start, 2);
        assert_eq!(converted_child.byte_end, 10);

        assert_eq!(converted_child.attributes.len(), 1);
        let converted_attr = &converted_child.attributes[0];
        assert_eq!(converted_attr.name, "class");
        assert_eq!(converted_attr.value, "primary");
        assert_eq!(converted_attr.byte_start, 3);
        assert_eq!(converted_attr.byte_end, 9);
    }

    /// `CodeDataNodeKind` must serialize under `snake_case` naming, matching the
    /// rest of xberg's public API convention.
    #[test]
    fn test_code_data_node_kind_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&CodeDataNodeKind::KeyValue).expect("serializes"),
            "\"key_value\""
        );
        assert_eq!(
            serde_json::to_string(&CodeDataNodeKind::Element).expect("serializes"),
            "\"element\""
        );
        assert_eq!(
            serde_json::to_string(&CodeDataNodeKind::Sequence).expect("serializes"),
            "\"sequence\""
        );
    }
}
