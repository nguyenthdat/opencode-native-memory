/// Postprocessing heuristics for filtering and resolving overlapping layout detections.
/// Engine-neutral — used by `LayoutEngine` regardless of engine.
pub mod heuristics;
#[cfg(feature = "layout-detection")]
/// Non-Maximum Suppression utilities for deduplicating overlapping bounding boxes.
/// ORT-only: RT-DETR is NMS-free, so this is used only by YOLO.
pub mod nms;
