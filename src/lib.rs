pub mod capture;
mod config;
mod contract;
pub mod daemon;
mod document;
mod document_index;
mod embedding;
mod embedding_generation;
mod engine;
mod graph;
mod lifecycle;
pub mod model;
pub mod rpc;
mod storage;
pub mod taxonomy;
mod validation;

#[allow(clippy::enum_variant_names)]
pub(crate) mod memory_proto {
    include!(concat!(env!("OUT_DIR"), "/opencode.memory.v1.rs"));
}

#[allow(clippy::enum_variant_names)]
pub(crate) mod model_proto {
    include!(concat!(env!("OUT_DIR"), "/opencode.memory.model.v1.rs"));
}

#[allow(clippy::enum_variant_names, clippy::large_enum_variant)]
pub(crate) mod graph_proto {
    include!(concat!(env!("OUT_DIR"), "/opencode.memory.graph.v1.rs"));
}

#[allow(clippy::enum_variant_names, clippy::large_enum_variant)]
pub(crate) mod daemon_proto {
    include!(concat!(env!("OUT_DIR"), "/opencode.memory.daemon.v1.rs"));
}

pub use capture::{
    AUTO_COMPACTION_CONFIDENCE_CAP, CaptureDecision, CaptureGate, CapturePlan, CaptureSafety,
    CaptureSignals, DEFAULT_ACTIONABILITY_THRESHOLD, DEFAULT_SIGNIFICANCE_THRESHOLD,
    MAX_SUGGESTED_RELATION_IDS, NoveltyDisposition, QuarantineReason, SkipReason, SourceTrust,
};
pub use config::{EmbeddingConfig, MemoryConfig};
pub use contract::{
    CaptureRequest, CaptureResponse, CodeAnchor, DeleteReason, DeleteRequest, DeleteResponse,
    DoctorRequest, DoctorResponse, DocumentIndexRejection, DocumentIndexRequest,
    DocumentIndexResponse, ExportRequest, FeedbackEvent, FeedbackRequest, FeedbackResponse,
    FeedbackStats, ForgetRequest, ForgetResponse, GetRequest, ImportRequest, ImportResponse,
    IndexStatus, IngestRequest, IngestResponse, LifecycleResponse, ListRequest, ListResponse,
    LockAction, LockRequest, MemoryKind, MemoryOrigin, MemoryRecord, MemoryScope, MemorySnapshot,
    OptimizeResponse, PinRequest, PurgeRequest, PurgeResponse, RetrievalMode, ScoreBreakdown,
    SearchRequest, SearchResponse, SharedMemoryInput, SharedMemoryRejection, StatusResponse,
    StoreRequest, StoreResponse, SyncSharedRequest, SyncSharedResponse, TombstoneSnapshot,
    UpdateRequest, UpdateResponse,
};
pub use engine::MemoryEngine;
pub use taxonomy::{MemoryFamily, MemoryTaxonomy, RetrievalProfile};
