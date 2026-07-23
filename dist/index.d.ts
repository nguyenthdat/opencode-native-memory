declare const _default: import("@opencode-ai/plugin").Plugin;
export default _default;
export { MEMORY_KINDS, MEMORY_SCOPES, MEMORY_TAXONOMIES, WRITABLE_MEMORY_SCOPES, FEEDBACK_EVENTS, LOCK_ACTIONS, LOCK_REASON_MAX, UNLOCK_FORBIDDEN_FIELDS, } from "./contracts.js";
export type { MemoryRecord, SearchResponse, ListResponse, PendingRecall, CuratedCandidate, SharedMemoryRecord, SharedSyncResponse, } from "./contracts.js";
export { NativeMemoryClient, resolveNativeMemoryBinary, REQUEST_TIMEOUT_MS, INITIALIZATION_TIMEOUT_MS, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, } from "./sidecar-client.js";
export type { SpawnFn } from "./sidecar-client.js";
export { decodeResponse, DelimitedFrameDecoder, encodeRequest } from "./protocol.js";
export type { MemoryMethod } from "./protocol.js";
export { createMemoryPlugin, resolveMemoryPluginOptions } from "./plugin.js";
export type { MemoryPluginOptions } from "./plugin.js";
export { SessionContext } from "./session-context.js";
export { validateUpdateArgs } from "./validation.js";
export { formatRecalledMemories, parseCuratedCandidates, truncateText, contextBudgetChars, safeJson, MEMORY_POLICY_MARKER, MEMORY_POLICY, COMPACTION_CONTEXT, CANDIDATES_OPEN, CANDIDATES_CLOSE, } from "./policy.js";
export { loadSharedMemories, parseSharedMemory, writeSharedMemory, SHARED_MEMORY_RELATIVE_DIR, } from "./shared-markdown.js";
//# sourceMappingURL=index.d.ts.map