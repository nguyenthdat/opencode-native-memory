import { fileURLToPath } from "node:url";
import { basename, dirname, resolve } from "node:path";
import { createMemoryPlugin } from "./plugin.js";

const moduleDirectory = dirname(fileURLToPath(import.meta.url));
const packageRoot =
  basename(moduleDirectory) === "dist"
    ? resolve(moduleDirectory, "..")
    : resolve(moduleDirectory, "../..");

export default createMemoryPlugin({ root: packageRoot });

// Contracts and constants
export {
  MEMORY_KINDS,
  MEMORY_SCOPES,
  MEMORY_TAXONOMIES,
  USER_PROFILE_TAXONOMIES,
  isUserProfileTaxonomy,
  RETRIEVAL_MODES,
  WRITABLE_MEMORY_SCOPES,
  FEEDBACK_EVENTS,
  LOCK_ACTIONS,
  LOCK_REASON_MAX,
  UNLOCK_FORBIDDEN_FIELDS,
} from "./contracts.js";

export type {
  MemoryRecord,
  SearchResponse,
  RetrievalMode,
  ListResponse,
  IngestResponse,
  DocumentIndexResponse,
  PendingRecall,
  CuratedCandidate,
  SharedMemoryRecord,
  SharedSyncResponse,
  NativeMemoryStatus,
  MemoryPluginHealthStatus,
  MemoryPluginHealthIssue,
  MemoryPluginHealth,
  MemoryStatusResponse,
  MemoryModelProfile,
  MemoryModelProfilesResponse,
  ModelProfileReason,
  ModelSwitchBlocker,
  ModelSwitchPreflight,
  ModelSwitchResponse,
  ModelSwitchStatusResponse,
  ModelSwitchCancelResponse,
} from "./contracts.js";

// Shared daemon client
export {
  NativeMemoryClient,
  NativeMemoryClientPool,
  DaemonOutcomeUnknownError,
  DaemonRpcError,
  probeNativeMemoryDaemon,
  resolveDaemonEndpoint,
  resolveNativeMemoryBinary,
  REQUEST_TIMEOUT_MS,
  INITIALIZATION_TIMEOUT_MS,
  MAX_REQUEST_BYTES,
  MAX_RESPONSE_BYTES,
} from "./daemon-client.js";
export type {
  DaemonClientInfo,
  DaemonControlInfo,
  NativeMemoryClientLease,
  NativeMemoryRequester,
} from "./daemon-client.js";

export {
  createModelRequest,
  createMemoryRequest,
  createProjectRequest,
  decodeMemoryResponse,
  decodeModelResponse,
  decodeResponse,
  DelimitedFrameDecoder,
  encodeDelimited,
  encodeRequest,
  isModelMethod,
} from "./protocol.js";
export type { MemoryMethod, ModelMethod, ProjectRequest } from "./protocol.js";

export {
  listModelProfiles,
  preflightModelSwitch,
  type ModelSwitchPreflightOptions,
} from "./model-control.js";
export {
  captureWithOutcomeReconciliation,
  type ReconciledCaptureResponse,
} from "./capture-reconciliation.js";
export {
  MemoryMaintenanceScheduler,
  DEFAULT_OPTIMIZE_DEBOUNCE_MS,
  DEFAULT_OPTIMIZE_INDEX_THRESHOLD,
  type MemoryMaintenanceOptions,
} from "./maintenance.js";
export {
  requestIdempotently,
  isOutcomeUnknown,
  type IdempotentMaintenanceMethod,
} from "./outcome-reconciliation.js";

// Plugin factory
export { createMemoryPlugin, resolveMemoryPluginOptions } from "./plugin.js";
export type { MemoryPluginOptions } from "./plugin.js";

// Session context (testable abstraction)
export { SessionContext } from "./session-context.js";

// Lifecycle validation
export { validateUpdateArgs } from "./validation.js";

// Managed instruction asset
export {
  MEMORY_INSTRUCTIONS_MARKER,
  loadMemoryInstructions,
  registerMemoryInstructions,
} from "./instructions.js";

// Policy helpers
export {
  formatRecalledMemories,
  parseCuratedCandidates,
  truncateText,
  contextBudgetChars,
  safeJson,
  COMPACTION_CONTEXT,
  CANDIDATES_OPEN,
  CANDIDATES_CLOSE,
} from "./policy.js";

// Shared-markdown helpers
export {
  loadSharedMemories,
  parseSharedMemory,
  writeSharedMemory,
  SHARED_MEMORY_RELATIVE_DIR,
} from "./shared-markdown.js";
