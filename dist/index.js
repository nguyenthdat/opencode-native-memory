import { fileURLToPath } from "node:url";
import { basename, dirname, resolve } from "node:path";
import { createMemoryPlugin } from "./plugin.js";
const moduleDirectory = dirname(fileURLToPath(import.meta.url));
const packageRoot = basename(moduleDirectory) === "dist"
    ? resolve(moduleDirectory, "..")
    : resolve(moduleDirectory, "../..");
export default createMemoryPlugin({ root: packageRoot });
// Contracts and constants
export { MEMORY_KINDS, MEMORY_SCOPES, MEMORY_TAXONOMIES, WRITABLE_MEMORY_SCOPES, FEEDBACK_EVENTS, LOCK_ACTIONS, LOCK_REASON_MAX, UNLOCK_FORBIDDEN_FIELDS, } from "./contracts.js";
// Sidecar client
export { NativeMemoryClient, resolveNativeMemoryBinary, REQUEST_TIMEOUT_MS, INITIALIZATION_TIMEOUT_MS, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, } from "./sidecar-client.js";
export { decodeResponse, DelimitedFrameDecoder, encodeRequest } from "./protocol.js";
// Plugin factory
export { createMemoryPlugin, resolveMemoryPluginOptions } from "./plugin.js";
// Session context (testable abstraction)
export { SessionContext } from "./session-context.js";
// Lifecycle validation
export { validateUpdateArgs } from "./validation.js";
// Managed instruction asset
export { MEMORY_INSTRUCTIONS_MARKER, loadMemoryInstructions, registerMemoryInstructions, } from "./instructions.js";
// Policy helpers
export { formatRecalledMemories, parseCuratedCandidates, truncateText, contextBudgetChars, safeJson, COMPACTION_CONTEXT, CANDIDATES_OPEN, CANDIDATES_CLOSE, } from "./policy.js";
// Shared-markdown helpers
export { loadSharedMemories, parseSharedMemory, writeSharedMemory, SHARED_MEMORY_RELATIVE_DIR, } from "./shared-markdown.js";
//# sourceMappingURL=index.js.map