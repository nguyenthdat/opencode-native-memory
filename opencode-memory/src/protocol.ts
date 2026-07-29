import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  Method,
  RequestSchema,
  ResponseSchema,
  ValueListSchema,
  ValueObjectSchema,
  ValueSchema,
} from "./generated/opencode/memory/v1/memory_pb.js";
import type { Request, Response, Value } from "./generated/opencode/memory/v1/memory_pb.js";
import {
  CancelModelSwitchRequestSchema,
  EmbeddingMetric,
  EmbeddingModality,
  GetModelSwitchStatusRequestSchema,
  ListModelProfilesRequestSchema,
  ModelCancelOutcome,
  ModelPreflightDecision,
  ModelProfileCapability,
  ModelProfileRole,
  ModelProfileSupportLevel,
  ModelRequestSchema,
  ModelStatusCode,
  ModelSwitchAvailability,
  ModelSwitchExecutionMode,
  ModelSwitchRebuildPolicy,
  ModelSwitchState,
  StartModelSwitchRequestSchema,
} from "./generated/opencode/memory/model/v1/model_pb.js";
import type {
  ListModelProfilesResponse,
  ModelProfile,
  ModelRequest,
  ModelResponse,
  ModelSwitchPreflight as ProtobufModelSwitchPreflight,
  StartModelSwitchResponse,
} from "./generated/opencode/memory/model/v1/model_pb.js";
import type {
  MemoryModelProfile,
  MemoryModelProfilesResponse,
  ModelProfileSupportLevel as ModelProfileSupportLevelContract,
  ModelSwitchCancelResponse,
  ModelSwitchPreflight,
  ModelSwitchResponse,
  ModelSwitchStatusResponse,
  RpcResponse,
} from "./contracts.js";

const MAX_VALUE_DEPTH = 64;

const MEMORY_METHODS = {
  search: Method.SEARCH,
  store: Method.STORE,
  capture: Method.CAPTURE,
  export: Method.EXPORT,
  import: Method.IMPORT,
  ingest: Method.INGEST,
  index_documents: Method.INDEX_DOCUMENTS,
  get: Method.GET,
  list: Method.LIST,
  update: Method.UPDATE,
  pin: Method.PIN,
  lock: Method.LOCK,
  delete: Method.DELETE,
  forget: Method.FORGET,
  purge: Method.PURGE,
  feedback: Method.FEEDBACK,
  sync_shared: Method.SYNC_SHARED,
  status: Method.STATUS,
  optimize: Method.OPTIMIZE,
  doctor: Method.DOCTOR,
  shutdown: Method.SHUTDOWN,
} as const;

export type ModelMethod =
  "model_profiles" | "model_switch" | "model_switch_status" | "model_switch_cancel";
export type MemoryMethod = keyof typeof MEMORY_METHODS | ModelMethod;

export type ProjectRequest =
  | { kind: "memory"; request: Request }
  | { kind: "model"; method: ModelMethod; modelRequest: ModelRequest };

export function encodeRequest(id: number, method: string, params: unknown): Uint8Array {
  return encodeDelimited(toBinary(RequestSchema, createMemoryRequest(id, method, params)));
}

export function createMemoryRequest(id: number, method: string, params: unknown): Request {
  validateRequestId(id);
  const methodValue = MEMORY_METHODS[method as keyof typeof MEMORY_METHODS];
  if (methodValue === undefined) {
    throw new Error(`Unknown memory method: ${method}`);
  }
  return create(RequestSchema, {
    id: BigInt(id),
    method: methodValue,
    params: encodeValue(params, 0),
  });
}

export function createModelRequest(id: number, method: ModelMethod, params: unknown): ModelRequest {
  validateRequestId(id);
  const values = requestParams(params, method);
  let operation: ModelRequest["operation"];
  switch (method) {
    case "model_profiles":
      operation = {
        case: "listProfiles",
        value: create(ListModelProfilesRequestSchema),
      };
      break;
    case "model_switch": {
      const switchId = optionalStringParam(values, "switch_id", method);
      const expectedActiveProfileId = optionalStringParam(
        values,
        "expected_active_profile_id",
        method,
      );
      operation = {
        case: "startSwitch",
        value: create(StartModelSwitchRequestSchema, {
          ...(switchId === undefined ? {} : { switchId }),
          targetProfileId: requiredStringParam(values, "target_profile_id", method),
          ...(expectedActiveProfileId === undefined ? {} : { expectedActiveProfileId }),
          availability: booleanParam(values, "allow_dense_downtime", method)
            ? ModelSwitchAvailability.ALLOW_DENSE_DOWNTIME
            : ModelSwitchAvailability.KEEP_OLD_DENSE,
          executionMode: booleanParam(values, "dry_run", method)
            ? ModelSwitchExecutionMode.DRY_RUN
            : ModelSwitchExecutionMode.APPLY,
          rebuildPolicy: booleanParam(values, "force_rebuild", method)
            ? ModelSwitchRebuildPolicy.FORCE_REBUILD
            : ModelSwitchRebuildPolicy.REJECT_ACTIVE_PROFILE,
        }),
      };
      break;
    }
    case "model_switch_status":
      operation = {
        case: "getSwitchStatus",
        value: create(GetModelSwitchStatusRequestSchema, {
          switchId: requiredStringParam(values, "switch_id", method),
        }),
      };
      break;
    case "model_switch_cancel":
      operation = {
        case: "cancelSwitch",
        value: create(CancelModelSwitchRequestSchema, {
          switchId: requiredStringParam(values, "switch_id", method),
        }),
      };
      break;
  }
  return create(ModelRequestSchema, { id: BigInt(id), operation });
}

export function createProjectRequest(id: number, method: string, params: unknown): ProjectRequest {
  if (isModelMethod(method)) {
    return { kind: "model", method, modelRequest: createModelRequest(id, method, params) };
  }
  return { kind: "memory", request: createMemoryRequest(id, method, params) };
}

export function isModelMethod(method: string): method is ModelMethod {
  return (
    method === "model_profiles" ||
    method === "model_switch" ||
    method === "model_switch_status" ||
    method === "model_switch_cancel"
  );
}

export function decodeResponse(payload: Uint8Array): RpcResponse {
  return decodeMemoryResponse(fromBinary(ResponseSchema, payload));
}

export function decodeMemoryResponse(response: Response): RpcResponse {
  const id = safeNumber(response.id, "response ID");
  return {
    id,
    ok: response.ok,
    result: response.result ? decodeValue(response.result, 0) : undefined,
    error: response.error || undefined,
  };
}

export function decodeModelResponse(response: ModelResponse, method: ModelMethod): RpcResponse {
  const id = safeNumber(response.id, "model response ID");
  const status = response.status;
  if (!status) throw new Error("Native memory daemon omitted the model response status");
  if (status.code !== ModelStatusCode.OK) {
    return {
      id,
      ok: false,
      error: status.message || `Native memory model operation failed (${status.code})`,
    };
  }

  const expectedCase = modelResultCase(method);
  if (response.result.case !== expectedCase) {
    throw new Error(
      `Native memory daemon returned ${response.result.case ?? "no"} result for ${method}`,
    );
  }

  let result: unknown;
  switch (response.result.case) {
    case "listProfiles":
      result = mapModelProfilesResponse(response.result.value);
      break;
    case "startSwitch":
      result = mapModelSwitchResponse(response.result.value);
      break;
    case "getSwitchStatus":
      result = {
        switch_id: response.result.value.switchId,
        state: modelSwitchState(response.result.value.state),
        active_profile_id: response.result.value.activeProfileId,
        target_profile_id: response.result.value.targetProfileId,
        active_generation_id: response.result.value.activeGenerationId,
        target_generation_id: response.result.value.targetGenerationId ?? null,
        completed_records: safeNumber(
          response.result.value.completedRecords,
          "model switch completed record count",
        ),
        total_records: safeNumber(
          response.result.value.totalRecords,
          "model switch total record count",
        ),
        error: response.result.value.error
          ? {
              code: response.result.value.error.code,
              message: response.result.value.error.message,
            }
          : null,
      } satisfies ModelSwitchStatusResponse;
      break;
    case "cancelSwitch":
      result = {
        switch_id: response.result.value.switchId,
        outcome: modelCancelOutcome(response.result.value.outcome),
      } satisfies ModelSwitchCancelResponse;
      break;
    case undefined:
      throw new Error(`Native memory daemon omitted the model result for ${method}`);
  }
  return { id, ok: true, result, error: undefined };
}

export class DelimitedFrameDecoder {
  private buffered = new Uint8Array(0);

  constructor(private readonly maxFrameBytes: number) {}

  push(chunk: Uint8Array): Uint8Array[] {
    if (chunk.byteLength === 0) return [];
    const combined = new Uint8Array(this.buffered.byteLength + chunk.byteLength);
    combined.set(this.buffered);
    combined.set(chunk, this.buffered.byteLength);
    this.buffered = combined;

    const frames: Uint8Array[] = [];
    let offset = 0;
    while (offset < this.buffered.byteLength) {
      const header = readVarint(this.buffered, offset);
      if (!header) break;
      if (header.value > this.maxFrameBytes) {
        throw new Error(
          `Memory response exceeds ${this.maxFrameBytes} bytes (declared ${header.value})`,
        );
      }
      const frameEnd = header.next + header.value;
      if (frameEnd > this.buffered.byteLength) break;
      frames.push(this.buffered.slice(header.next, frameEnd));
      offset = frameEnd;
    }
    if (offset > 0) this.buffered = this.buffered.slice(offset);
    if (this.buffered.byteLength > this.maxFrameBytes + 10) {
      throw new Error(`Memory response exceeds ${this.maxFrameBytes} bytes`);
    }
    return frames;
  }
}

function encodeValue(input: unknown, depth: number): Value {
  if (depth > MAX_VALUE_DEPTH) {
    throw new Error("Memory request value nesting exceeds limit");
  }
  if (input === null || input === undefined) {
    return create(ValueSchema, {
      kind: { case: "nullValue", value: true },
    });
  }
  if (typeof input === "boolean") {
    return create(ValueSchema, {
      kind: { case: "booleanValue", value: input },
    });
  }
  if (typeof input === "bigint") {
    if (input < BigInt(Number.MIN_SAFE_INTEGER) || input > BigInt(Number.MAX_SAFE_INTEGER)) {
      throw new Error("Memory request contains an integer outside the JavaScript safe range");
    }
    return create(ValueSchema, {
      kind:
        input >= 0n
          ? { case: "unsignedValue", value: input }
          : { case: "signedValue", value: input },
    });
  }
  if (typeof input === "number") {
    if (!Number.isFinite(input)) {
      throw new Error("Memory request contains a non-finite number");
    }
    if (Number.isInteger(input) && !Number.isSafeInteger(input)) {
      throw new Error("Memory request contains an integer outside the JavaScript safe range");
    }
    if (Number.isSafeInteger(input)) {
      return create(ValueSchema, {
        kind:
          input >= 0
            ? { case: "unsignedValue", value: BigInt(input) }
            : { case: "signedValue", value: BigInt(input) },
      });
    }
    return create(ValueSchema, {
      kind: { case: "floatValue", value: input },
    });
  }
  if (typeof input === "string") {
    return create(ValueSchema, {
      kind: { case: "textValue", value: input },
    });
  }
  if (Array.isArray(input)) {
    return create(ValueSchema, {
      kind: {
        case: "listValue",
        value: create(ValueListSchema, {
          values: input.map((value) => encodeValue(value, depth + 1)),
        }),
      },
    });
  }
  if (typeof input === "object") {
    const fields: Record<string, Value> = {};
    for (const [key, value] of Object.entries(input)) {
      if (value !== undefined) fields[key] = encodeValue(value, depth + 1);
    }
    return create(ValueSchema, {
      kind: {
        case: "objectValue",
        value: create(ValueObjectSchema, { fields }),
      },
    });
  }
  throw new Error(`Unsupported memory request value: ${typeof input}`);
}

function decodeValue(input: Value, depth: number): unknown {
  if (depth > MAX_VALUE_DEPTH) {
    throw new Error("Memory response value nesting exceeds limit");
  }
  switch (input.kind.case) {
    case "booleanValue":
    case "floatValue":
    case "textValue":
      return input.kind.value;
    case "signedValue":
    case "unsignedValue":
      return safeNumber(input.kind.value, "response integer");
    case "listValue":
      return input.kind.value.values.map((value) => decodeValue(value, depth + 1));
    case "objectValue":
      return Object.fromEntries(
        Object.entries(input.kind.value.fields).map(([key, value]) => [
          key,
          decodeValue(value, depth + 1),
        ]),
      );
    case "nullValue":
    case undefined:
      return null;
  }
}

function mapModelProfilesResponse(
  response: ListModelProfilesResponse,
): MemoryModelProfilesResponse {
  return {
    catalog_version: response.catalogVersion,
    catalog_digest: response.catalogDigest,
    active_profile_id: response.activeProfileId,
    active_generation_id: response.activeGenerationId,
    profiles: response.profiles.map(mapModelProfile),
  };
}

function mapModelProfile(profile: ModelProfile): MemoryModelProfile {
  return {
    profile_id: profile.profileId,
    display_name: profile.displayName,
    description: profile.description,
    modalities: profile.modalities.map(modelModality),
    repository: profile.repository ?? null,
    filename: profile.filename ?? null,
    revision: profile.revision ?? null,
    artifact_sha256: profile.artifactSha256 ?? null,
    runtime_family: profile.runtimeFamily,
    dimension: profile.dimension ?? null,
    metric: modelMetric(profile.metric),
    support_level: modelSupportLevel(profile.supportLevel),
    selectable: profile.capabilities.includes(ModelProfileCapability.SELECTABLE),
    default_for_new_projects: profile.roles.includes(ModelProfileRole.DEFAULT_FOR_NEW_PROJECTS),
    recommended: profile.roles.includes(ModelProfileRole.RECOMMENDED),
    installed: profile.capabilities.includes(ModelProfileCapability.INSTALLED),
    platform_supported: profile.capabilities.includes(ModelProfileCapability.PLATFORM_SUPPORTED),
    runtime_available: profile.capabilities.includes(ModelProfileCapability.RUNTIME_AVAILABLE),
    artifact_locked: profile.capabilities.includes(ModelProfileCapability.ARTIFACT_LOCKED),
    estimated_download_bytes: optionalSafeNumber(
      profile.estimatedDownloadBytes,
      "model profile estimated download bytes",
    ),
    estimated_resident_bytes: optionalSafeNumber(
      profile.estimatedResidentBytes,
      "model profile estimated resident bytes",
    ),
    unavailable_reason: profile.unavailableReason
      ? { code: profile.unavailableReason.code, message: profile.unavailableReason.message }
      : null,
  };
}

function mapModelSwitchResponse(response: StartModelSwitchResponse): ModelSwitchResponse {
  if (!response.preflight) {
    throw new Error("Native memory daemon omitted the model switch preflight");
  }
  const preflight = mapModelSwitchPreflight(response.preflight);
  return {
    switch_id: response.switchId ?? null,
    dry_run: modelSwitchDryRun(response.executionMode),
    state: modelSwitchState(response.state),
    active_profile_id: response.activeProfileId,
    target_profile_id: response.targetProfileId,
    active_generation_id: response.activeGenerationId,
    target_generation_id: response.targetGenerationId ?? null,
    dense_search_available: response.denseSearchAvailable,
    preflight,
  };
}

function mapModelSwitchPreflight(preflight: ProtobufModelSwitchPreflight): ModelSwitchPreflight {
  const availability = modelSwitchAvailability(preflight.availability);
  return {
    can_start: modelPreflightCanStart(preflight.decision),
    availability,
    dense_search_available: preflight.denseSearchAvailable,
    estimated_download_bytes: optionalSafeNumber(
      preflight.estimatedDownloadBytes,
      "model switch estimated download bytes",
    ),
    estimated_disk_bytes: optionalSafeNumber(
      preflight.estimatedDiskBytes,
      "model switch estimated disk bytes",
    ),
    estimated_resident_bytes: optionalSafeNumber(
      preflight.estimatedResidentBytes,
      "model switch estimated resident bytes",
    ),
    warnings: [...preflight.warnings],
    blockers: preflight.blockers.map((blocker) => ({
      code: blocker.code,
      message: blocker.message,
    })),
  };
}

function modelResultCase(method: ModelMethod): Exclude<ModelResponse["result"]["case"], undefined> {
  switch (method) {
    case "model_profiles":
      return "listProfiles";
    case "model_switch":
      return "startSwitch";
    case "model_switch_status":
      return "getSwitchStatus";
    case "model_switch_cancel":
      return "cancelSwitch";
  }
}

function modelModality(value: EmbeddingModality): string {
  switch (value) {
    case EmbeddingModality.TEXT:
      return "text";
    case EmbeddingModality.IMAGE:
      return "image";
    case EmbeddingModality.MIXED:
      return "mixed";
    case EmbeddingModality.UNSPECIFIED:
      throw new Error("Native memory daemon returned an unspecified model modality");
    default:
      throw new Error(`Native memory daemon returned unknown model modality ${value}`);
  }
}

function modelMetric(value: EmbeddingMetric): MemoryModelProfile["metric"] {
  switch (value) {
    case EmbeddingMetric.UNSPECIFIED:
      return null;
    case EmbeddingMetric.COSINE:
      return "cosine";
    case EmbeddingMetric.DOT_PRODUCT:
      return "dot_product";
    default:
      throw new Error(`Native memory daemon returned unknown model metric ${value}`);
  }
}

function modelSupportLevel(value: ModelProfileSupportLevel): ModelProfileSupportLevelContract {
  switch (value) {
    case ModelProfileSupportLevel.STABLE:
      return "stable";
    case ModelProfileSupportLevel.PREVIEW:
      return "preview";
    case ModelProfileSupportLevel.UNSUPPORTED:
      return "unsupported";
    case ModelProfileSupportLevel.UNSPECIFIED:
      throw new Error("Native memory daemon returned an unspecified model support level");
    default:
      throw new Error(`Native memory daemon returned unknown model support level ${value}`);
  }
}

function modelSwitchAvailability(
  value: ModelSwitchAvailability,
): ModelSwitchPreflight["availability"] {
  switch (value) {
    case ModelSwitchAvailability.KEEP_OLD_DENSE:
      return "keep_old_dense";
    case ModelSwitchAvailability.ALLOW_DENSE_DOWNTIME:
      return "allow_dense_downtime";
    case ModelSwitchAvailability.UNSPECIFIED:
      throw new Error("Native memory daemon returned an unspecified model switch availability");
    default:
      throw new Error(`Native memory daemon returned unknown model switch availability ${value}`);
  }
}

function modelSwitchDryRun(value: ModelSwitchExecutionMode): boolean {
  switch (value) {
    case ModelSwitchExecutionMode.APPLY:
      return false;
    case ModelSwitchExecutionMode.DRY_RUN:
      return true;
    case ModelSwitchExecutionMode.UNSPECIFIED:
      throw new Error("Native memory daemon returned an unspecified model switch execution mode");
    default:
      throw new Error(`Native memory daemon returned unknown model switch execution mode ${value}`);
  }
}

function modelPreflightCanStart(value: ModelPreflightDecision): boolean {
  switch (value) {
    case ModelPreflightDecision.READY:
      return true;
    case ModelPreflightDecision.BLOCKED:
      return false;
    case ModelPreflightDecision.UNSPECIFIED:
      throw new Error("Native memory daemon returned an unspecified model preflight decision");
    default:
      throw new Error(`Native memory daemon returned unknown model preflight decision ${value}`);
  }
}

function modelSwitchState(value: ModelSwitchState): ModelSwitchResponse["state"] {
  switch (value) {
    case ModelSwitchState.PREFLIGHT:
      return "preflight";
    case ModelSwitchState.QUEUED:
      return "queued";
    case ModelSwitchState.VALIDATING:
      return "validating";
    case ModelSwitchState.DOWNLOADING:
      return "downloading";
    case ModelSwitchState.PREPARING:
      return "preparing";
    case ModelSwitchState.REINDEXING:
      return "reindexing";
    case ModelSwitchState.VERIFYING:
      return "verifying";
    case ModelSwitchState.COMMITTING:
      return "committing";
    case ModelSwitchState.SUCCEEDED:
      return "succeeded";
    case ModelSwitchState.CANCEL_REQUESTED:
      return "cancel_requested";
    case ModelSwitchState.CANCELLED:
      return "cancelled";
    case ModelSwitchState.FAILED:
      return "failed";
    case ModelSwitchState.UNSPECIFIED:
      throw new Error("Native memory daemon returned an unspecified model switch state");
    default:
      throw new Error(`Native memory daemon returned unknown model switch state ${value}`);
  }
}

function modelCancelOutcome(value: ModelCancelOutcome): ModelSwitchCancelResponse["outcome"] {
  switch (value) {
    case ModelCancelOutcome.CANCEL_REQUESTED:
      return "cancel_requested";
    case ModelCancelOutcome.CANCELLED_BEFORE_COMMIT:
      return "cancelled_before_commit";
    case ModelCancelOutcome.ALREADY_COMMITTING:
      return "already_committing";
    case ModelCancelOutcome.ALREADY_COMMITTED:
      return "already_committed";
    case ModelCancelOutcome.ALREADY_TERMINAL:
      return "already_terminal";
    case ModelCancelOutcome.NOT_FOUND:
      return "not_found";
    case ModelCancelOutcome.UNSPECIFIED:
      throw new Error("Native memory daemon returned an unspecified model cancel outcome");
    default:
      throw new Error(`Native memory daemon returned unknown model cancel outcome ${value}`);
  }
}

function validateRequestId(id: number): void {
  if (!Number.isSafeInteger(id) || id <= 0) {
    throw new Error(`Invalid memory request ID: ${id}`);
  }
}

function requestParams(params: unknown, method: ModelMethod): Record<string, unknown> {
  if (params === undefined || params === null) return {};
  if (typeof params !== "object" || Array.isArray(params)) {
    throw new Error(`Memory ${method} parameters must be an object`);
  }
  return params as Record<string, unknown>;
}

function requiredStringParam(
  params: Record<string, unknown>,
  key: string,
  method: ModelMethod,
): string {
  const value = params[key];
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`Memory ${method} requires a non-empty ${key}`);
  }
  return value;
}

function optionalStringParam(
  params: Record<string, unknown>,
  key: string,
  method: ModelMethod,
): string | undefined {
  const value = params[key];
  if (value === undefined || value === null) return undefined;
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`Memory ${method} requires ${key} to be a non-empty string when provided`);
  }
  return value;
}

function booleanParam(params: Record<string, unknown>, key: string, method: ModelMethod): boolean {
  const value = params[key];
  if (value === undefined || value === null) return false;
  if (typeof value !== "boolean") {
    throw new Error(`Memory ${method} requires ${key} to be a boolean when provided`);
  }
  return value;
}

function optionalSafeNumber(value: bigint | undefined, label: string): number | null {
  return value === undefined ? null : safeNumber(value, label);
}

function safeNumber(value: bigint, label: string): number {
  const number = Number(value);
  if (!Number.isSafeInteger(number)) {
    throw new Error(`Memory ${label} exceeds JavaScript's safe integer range`);
  }
  return number;
}

export function encodeDelimited(payload: Uint8Array): Uint8Array {
  const header = encodeVarint(payload.byteLength);
  const frame = new Uint8Array(header.byteLength + payload.byteLength);
  frame.set(header);
  frame.set(payload, header.byteLength);
  return frame;
}

function encodeVarint(value: number): Uint8Array {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`Invalid Protobuf frame length: ${value}`);
  }
  const bytes: number[] = [];
  let remaining = value;
  do {
    let byte = remaining % 128;
    remaining = Math.floor(remaining / 128);
    if (remaining > 0) byte |= 0x80;
    bytes.push(byte);
  } while (remaining > 0);
  return Uint8Array.from(bytes);
}

function readVarint(
  bytes: Uint8Array,
  offset: number,
): { value: number; next: number } | undefined {
  let value = 0;
  let multiplier = 1;
  for (let index = offset; index < bytes.byteLength && index < offset + 10; index += 1) {
    const byte = bytes[index]!;
    value += (byte & 0x7f) * multiplier;
    if (!Number.isSafeInteger(value)) {
      throw new Error("Invalid Protobuf frame length");
    }
    if ((byte & 0x80) === 0) return { value, next: index + 1 };
    multiplier *= 128;
  }
  if (bytes.byteLength - offset >= 10) {
    throw new Error("Invalid Protobuf frame length");
  }
  return undefined;
}
