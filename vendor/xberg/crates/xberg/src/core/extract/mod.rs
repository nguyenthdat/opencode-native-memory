//! Unified public extraction API.
//!
//! These functions are the stable, binding-generated public surface. Their
//! signatures must remain byte-identical (they are scanned by the alef binding
//! generator). The implementation delegates to a process-global default
//! [`crate::engine::Engine`]; the extraction internals live in
//! [`crate::engine`] and are a pure refactor of what previously lived here.

use std::sync::LazyLock;

use crate::Result;
#[cfg(feature = "url-ingestion")]
use crate::core::config::UrlExtractionConfig;
use crate::core::config::{ExtractInput, ExtractionConfig, ExtractionResult};
#[cfg(feature = "url-ingestion")]
use crawlberg::{CrawlEngine, MapResult};

/// Process-global default engine backing the free `extract` / `extract_batch`
/// functions. Construction is cheap and side-effect free.
static DEFAULT_ENGINE: LazyLock<crate::engine::Engine> = LazyLock::new(crate::engine::Engine::new_default);

/// Extract content from a single bytes or URI input.
pub async fn extract(input: ExtractInput, config: &ExtractionConfig) -> Result<ExtractionResult> {
    DEFAULT_ENGINE.extract(input, config).await
}

/// Extract content from multiple bytes or URI inputs.
pub async fn extract_batch(inputs: Vec<ExtractInput>, config: &ExtractionConfig) -> Result<ExtractionResult> {
    DEFAULT_ENGINE.extract_batch(inputs, config).await
}

/// Discover all pages and sitemaps reachable from `uri` without extracting document content.
///
/// Builds a [`crawlberg::CrawlEngine`] from `config.crawl`, calls
/// [`CrawlEngine::map`], and returns the set of discovered URLs as a
/// [`crawlberg::MapResult`] (re-exported as [`crate::MapResult`]).
///
/// Use this when you need the URL inventory of a site before committing to
/// full document extraction — e.g. to build a crawl queue or validate scope.
///
/// # Errors
///
/// Returns [`crate::XbergError::Validation`] if the crawl configuration fails
/// validation or if the map operation itself fails.
#[cfg(feature = "url-ingestion")]
pub async fn map_url(uri: &str, config: &UrlExtractionConfig) -> Result<MapResult> {
    config.crawl.validate().map_err(map_crawl_err)?;
    let engine = CrawlEngine::builder()
        .config(config.crawl.clone())
        .build()
        .map_err(map_crawl_err)?;
    engine.map(uri).await.map_err(map_crawl_err)
}

/// Convert a [`crawlberg::CrawlError`] into an [`crate::XbergError`].
///
/// Mirrors the conversion used by the URL-ingestion extraction paths.
#[cfg(feature = "url-ingestion")]
fn map_crawl_err(error: crawlberg::CrawlError) -> crate::XbergError {
    crate::XbergError::validation(format!("crawlberg URL extraction failed: {error}"))
}
