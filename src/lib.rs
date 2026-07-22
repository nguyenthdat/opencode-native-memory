pub mod capture;
mod config;
mod contract;
mod engine;
mod lifecycle;
pub mod rpc;
mod storage;
pub mod taxonomy;
mod validation;

pub use capture::{
    AUTO_COMPACTION_CONFIDENCE_CAP, CaptureDecision, CaptureGate, CapturePlan, CaptureSafety,
    CaptureSignals, DEFAULT_ACTIONABILITY_THRESHOLD, DEFAULT_SIGNIFICANCE_THRESHOLD,
    MAX_SUGGESTED_RELATION_IDS, NoveltyDisposition, QuarantineReason, SkipReason, SourceTrust,
};
pub use config::MemoryConfig;
pub use contract::{
    CodeAnchor, DeleteReason, DeleteRequest, DeleteResponse, DoctorRequest, DoctorResponse,
    FeedbackEvent, FeedbackRequest, FeedbackResponse, FeedbackStats, ForgetRequest, ForgetResponse,
    GetRequest, IndexStatus, LifecycleResponse, ListRequest, ListResponse, LockAction, LockRequest,
    MemoryKind, MemoryOrigin, MemoryRecord, MemoryScope, OptimizeResponse, PinRequest,
    PurgeRequest, PurgeResponse, ScoreBreakdown, SearchRequest, SearchResponse, SharedMemoryInput,
    StatusResponse, StoreRequest, StoreResponse, SyncSharedRequest, SyncSharedResponse,
    UpdateRequest, UpdateResponse,
};
pub use engine::MemoryEngine;
pub use taxonomy::{MemoryFamily, MemoryTaxonomy, RetrievalProfile};
