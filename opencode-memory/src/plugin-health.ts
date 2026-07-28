import type {
  DocumentIndexResponse,
  MemoryPluginHealth,
  MemoryPluginHealthIssue,
  MemoryStatusResponse,
  NativeMemoryStatus,
} from "./contracts.js";

export function buildMemoryStatusResponse(
  backend: PromiseSettledResult<NativeMemoryStatus>,
  sharedSync: PromiseSettledResult<void>,
  documentIndex: PromiseSettledResult<DocumentIndexResponse | undefined>,
  checkedAtMs = Date.now(),
): MemoryStatusResponse {
  const issues: MemoryPluginHealthIssue[] = [];

  if (backend.status === "rejected") {
    issues.push({ component: "backend", message: errorMessage(backend.reason) });
  } else if (!backend.value.ready) {
    issues.push({ component: "backend", message: "Native memory backend is not ready" });
  } else {
    for (const index of backend.value.indexes) {
      if (index.completeness < 1) {
        issues.push({
          component: "backend",
          message: `Index ${index.name} is ${(index.completeness * 100).toFixed(1)}% complete`,
        });
      }
    }
    if (backend.value.pending_upsert_count > 0) {
      const count = backend.value.pending_upsert_count;
      issues.push({
        component: "backend",
        message: `${count} memory upsert${count === 1 ? "" : "s"} ${count === 1 ? "is" : "are"} pending recovery`,
      });
    }
    if (backend.value.pending_delete_count > 0) {
      const count = backend.value.pending_delete_count;
      issues.push({
        component: "backend",
        message: `${count} memory delete${count === 1 ? "" : "s"} ${count === 1 ? "is" : "are"} pending recovery`,
      });
    }
  }

  if (sharedSync.status === "rejected") {
    issues.push({ component: "shared_sync", message: errorMessage(sharedSync.reason) });
  }

  if (documentIndex.status === "rejected") {
    issues.push({ component: "document_index", message: errorMessage(documentIndex.reason) });
  } else if (documentIndex.value) {
    for (const rejection of documentIndex.value.rejections) {
      issues.push({
        component: "document_index",
        message: `${rejection.path}: ${rejection.message}`,
      });
    }
    for (const warning of documentIndex.value.warnings) {
      issues.push({ component: "document_index", message: warning });
    }
  }

  const ready = backend.status === "fulfilled" && backend.value.ready;
  const health: MemoryPluginHealth = {
    status: ready ? (issues.length > 0 ? "degraded" : "healthy") : "unavailable",
    ready,
    checked_at_ms: checkedAtMs,
    issues,
  };

  return backend.status === "fulfilled"
    ? { ...backend.value, plugin_health: health }
    : { plugin_health: health };
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string" && error.length > 0) return error;
  return "Unknown health check failure";
}
