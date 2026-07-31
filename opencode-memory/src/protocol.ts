import { create, fromBinary, fromJson, toBinary } from "@bufbuild/protobuf";
import type { DescMessage, JsonObject, JsonValue, Message } from "@bufbuild/protobuf";
import { isReflectList, isReflectMap, isReflectMessage, reflect } from "@bufbuild/protobuf/reflect";
import { FeatureSet_FieldPresence } from "@bufbuild/protobuf/wkt";
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
import {
  GraphExtractPrepareRequestSchema,
  GraphExtractPrepareResponseSchema,
  GraphExtractCancelRequestSchema,
  GraphExtractCancelResponseSchema,
  GraphExtractClaimRequestSchema,
  GraphExtractClaimResponseSchema,
  GraphExtractEnqueueRequestSchema,
  GraphExtractEnqueueResponseSchema,
  GraphExtractFinishRequestSchema,
  GraphExtractFinishResponseSchema,
  GraphExtractJobStatusRequestSchema,
  GraphExtractJobStatusResponseSchema,
  GraphExtractRenewRequestSchema,
  GraphExtractRenewResponseSchema,
  GraphObservationActionRequestSchema,
  GraphObservationActionResponseSchema,
  GraphExportRequestSchema,
  GraphExportResponseSchema,
  GraphRequestSchema,
  GraphRunStatusRequestSchema,
  GraphRunStatusResponseSchema,
  GraphSearchRequestSchema,
  GraphSearchResponseSchema,
  GraphStatusCode,
  GraphStatusRequestSchema,
  GraphStatusResponseSchema,
  GraphUpsertCandidatesRequestSchema,
  GraphUpsertCandidatesResponseSchema,
} from "./generated/opencode/memory/graph/v1/graph_pb.js";
import type { GraphRequest, GraphResponse } from "./generated/opencode/memory/graph/v1/graph_pb.js";
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
export type GraphMethod =
  | "graph_extract_prepare"
  | "graph_upsert_candidates"
  | "graph_run_status"
  | "graph_extract_enqueue"
  | "graph_extract_claim"
  | "graph_extract_renew"
  | "graph_extract_complete"
  | "graph_extract_fail"
  | "graph_extract_finish"
  | "graph_extract_job_status"
  | "graph_extract_cancel"
  | "graph_observation_action"
  | "graph_search"
  | "graph_status"
  | "graph_export";
export type MemoryMethod = keyof typeof MEMORY_METHODS | ModelMethod | GraphMethod;

export type ProjectRequest =
  | { kind: "memory"; request: Request }
  | { kind: "model"; method: ModelMethod; modelRequest: ModelRequest }
  | { kind: "graph"; method: GraphMethod; graphRequest: GraphRequest };

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
      const expectedActiveGenerationId = optionalStringParam(
        values,
        "expected_active_generation_id",
        method,
      );
      const targetGenerationId = optionalStringParam(values, "target_generation_id", method);
      operation = {
        case: "startSwitch",
        value: create(StartModelSwitchRequestSchema, {
          ...(switchId === undefined ? {} : { switchId }),
          targetProfileId: requiredStringParam(values, "target_profile_id", method),
          ...(expectedActiveProfileId === undefined ? {} : { expectedActiveProfileId }),
          ...(expectedActiveGenerationId === undefined ? {} : { expectedActiveGenerationId }),
          availability: booleanParam(values, "allow_dense_downtime", method)
            ? ModelSwitchAvailability.ALLOW_DENSE_DOWNTIME
            : ModelSwitchAvailability.KEEP_OLD_DENSE,
          executionMode: booleanParam(values, "dry_run", method)
            ? ModelSwitchExecutionMode.DRY_RUN
            : ModelSwitchExecutionMode.APPLY,
          rebuildPolicy: booleanParam(values, "force_rebuild", method)
            ? ModelSwitchRebuildPolicy.FORCE_REBUILD
            : ModelSwitchRebuildPolicy.REJECT_ACTIVE_PROFILE,
          retainPrevious:
            values.retain_previous === undefined
              ? true
              : booleanParam(values, "retain_previous", method),
          ...(targetGenerationId === undefined ? {} : { targetGenerationId }),
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

export function createGraphRequest(id: number, method: GraphMethod, params: unknown): GraphRequest {
  validateRequestId(id);
  const values = graphRequestParams(params, method);
  let operation: GraphRequest["operation"];
  switch (method) {
    case "graph_extract_prepare":
      operation = {
        case: "extractPrepare",
        value: fromJson(GraphExtractPrepareRequestSchema, values),
      };
      break;
    case "graph_upsert_candidates":
      operation = {
        case: "upsertCandidates",
        value: fromJson(GraphUpsertCandidatesRequestSchema, values),
      };
      break;
    case "graph_run_status":
      operation = {
        case: "runStatus",
        value: fromJson(GraphRunStatusRequestSchema, values),
      };
      break;
    case "graph_search":
      operation = {
        case: "search",
        value: fromJson(GraphSearchRequestSchema, values),
      };
      break;
    case "graph_status":
      operation = {
        case: "status",
        value: fromJson(GraphStatusRequestSchema, values),
      };
      break;
    case "graph_export":
      operation = {
        case: "export",
        value: fromJson(GraphExportRequestSchema, values),
      };
      break;
    case "graph_extract_enqueue":
      operation = {
        case: "extractEnqueue",
        value: fromJson(GraphExtractEnqueueRequestSchema, values),
      };
      break;
    case "graph_extract_claim":
      operation = {
        case: "extractClaim",
        value: fromJson(GraphExtractClaimRequestSchema, values),
      };
      break;
    case "graph_extract_renew":
      operation = {
        case: "extractRenew",
        value: fromJson(GraphExtractRenewRequestSchema, values),
      };
      break;
    case "graph_extract_complete":
      operation = {
        case: "extractFinish",
        value: fromJson(GraphExtractFinishRequestSchema, {
          ...values,
          outcome: "GRAPH_EXTRACT_FINISH_OUTCOME_COMPLETED",
        }),
      };
      break;
    case "graph_extract_fail": {
      const retryable = values.retryable !== false;
      const finishValues = { ...values };
      delete finishValues.retryable;
      operation = {
        case: "extractFinish",
        value: fromJson(GraphExtractFinishRequestSchema, {
          ...finishValues,
          outcome: retryable
            ? "GRAPH_EXTRACT_FINISH_OUTCOME_RETRYABLE_FAILURE"
            : "GRAPH_EXTRACT_FINISH_OUTCOME_PERMANENT_FAILURE",
        }),
      };
      break;
    }
    case "graph_extract_finish":
      operation = {
        case: "extractFinish",
        value: fromJson(GraphExtractFinishRequestSchema, values),
      };
      break;
    case "graph_extract_job_status":
      operation = {
        case: "extractJobStatus",
        value: fromJson(GraphExtractJobStatusRequestSchema, values),
      };
      break;
    case "graph_extract_cancel":
      operation = {
        case: "extractCancel",
        value: fromJson(GraphExtractCancelRequestSchema, values),
      };
      break;
    case "graph_observation_action":
      operation = {
        case: "observationAction",
        value: fromJson(GraphObservationActionRequestSchema, values),
      };
      break;
  }
  return create(GraphRequestSchema, { id: BigInt(id), operation });
}

export function createProjectRequest(id: number, method: string, params: unknown): ProjectRequest {
  if (isModelMethod(method)) {
    return { kind: "model", method, modelRequest: createModelRequest(id, method, params) };
  }
  if (isGraphMethod(method)) {
    return { kind: "graph", method, graphRequest: createGraphRequest(id, method, params) };
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

export function isGraphMethod(method: string): method is GraphMethod {
  return (
    method === "graph_extract_prepare" ||
    method === "graph_upsert_candidates" ||
    method === "graph_run_status" ||
    method === "graph_extract_enqueue" ||
    method === "graph_extract_claim" ||
    method === "graph_extract_renew" ||
    method === "graph_extract_complete" ||
    method === "graph_extract_fail" ||
    method === "graph_extract_finish" ||
    method === "graph_extract_job_status" ||
    method === "graph_extract_cancel" ||
    method === "graph_observation_action" ||
    method === "graph_search" ||
    method === "graph_status" ||
    method === "graph_export"
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
        fraction: response.result.value.fraction,
        cancel_requested: response.result.value.cancelRequested,
        dense_search_available: response.result.value.denseSearchAvailable,
        created_at_ms: safeNumber(response.result.value.createdAtMs, "model switch created time"),
        updated_at_ms: safeNumber(response.result.value.updatedAtMs, "model switch updated time"),
        completed_at_ms:
          response.result.value.completedAtMs === undefined
            ? null
            : safeNumber(response.result.value.completedAtMs, "model switch completion time"),
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

export function decodeGraphResponse(response: GraphResponse, method: GraphMethod): RpcResponse {
  const id = safeNumber(response.id, "graph response ID");
  const status = response.status;
  if (!status) throw new Error("Native memory daemon omitted the graph response status");
  if (status.code !== GraphStatusCode.OK) {
    return {
      id,
      ok: false,
      error: status.message || `Native memory graph operation failed (${status.code})`,
    };
  }

  const expectedCase = graphResultCase(method);
  if (response.result.case !== expectedCase) {
    throw new Error(
      `Native memory daemon returned ${response.result.case ?? "no"} result for ${method}`,
    );
  }

  let result: unknown;
  switch (response.result.case) {
    case "extractPrepare":
      result = graphMessageObject(GraphExtractPrepareResponseSchema, response.result.value);
      break;
    case "upsertCandidates":
      result = graphMessageObject(GraphUpsertCandidatesResponseSchema, response.result.value);
      break;
    case "runStatus":
      result = graphMessageObject(GraphRunStatusResponseSchema, response.result.value);
      break;
    case "search":
      result = graphMessageObject(GraphSearchResponseSchema, response.result.value);
      break;
    case "graphStatus":
      result = graphMessageObject(GraphStatusResponseSchema, response.result.value);
      break;
    case "export":
      result = graphMessageObject(GraphExportResponseSchema, response.result.value);
      break;
    case "extractEnqueue":
      result = graphJobResponseObject(GraphExtractEnqueueResponseSchema, response.result.value);
      break;
    case "extractClaim":
      result = graphJobResponseObject(GraphExtractClaimResponseSchema, response.result.value);
      break;
    case "extractRenew":
      result = graphJobResponseObject(GraphExtractRenewResponseSchema, response.result.value);
      break;
    case "extractFinish":
      result = graphJobResponseObject(GraphExtractFinishResponseSchema, response.result.value);
      break;
    case "extractJobStatus":
      result = graphJobResponseObject(GraphExtractJobStatusResponseSchema, response.result.value);
      break;
    case "extractCancel":
      result = graphJobResponseObject(GraphExtractCancelResponseSchema, response.result.value);
      break;
    case "observationAction":
      result = graphMessageObject(GraphObservationActionResponseSchema, response.result.value);
      break;
    case undefined:
      throw new Error(`Native memory daemon omitted the graph result for ${method}`);
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

function graphResultCase(method: GraphMethod): Exclude<GraphResponse["result"]["case"], undefined> {
  switch (method) {
    case "graph_extract_prepare":
      return "extractPrepare";
    case "graph_upsert_candidates":
      return "upsertCandidates";
    case "graph_run_status":
      return "runStatus";
    case "graph_search":
      return "search";
    case "graph_status":
      return "graphStatus";
    case "graph_export":
      return "export";
    case "graph_extract_enqueue":
      return "extractEnqueue";
    case "graph_extract_claim":
      return "extractClaim";
    case "graph_extract_renew":
      return "extractRenew";
    case "graph_extract_complete":
    case "graph_extract_fail":
    case "graph_extract_finish":
      return "extractFinish";
    case "graph_extract_job_status":
      return "extractJobStatus";
    case "graph_extract_cancel":
      return "extractCancel";
    case "graph_observation_action":
      return "observationAction";
  }
}

function graphRequestParams(params: unknown, method: GraphMethod): JsonObject {
  const value = graphJsonValue(params, `Memory ${method} parameters`, 0, new Set());
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`Memory ${method} parameters must be an object`);
  }

  const authorization = requiredGraphObjectParam(value, "authorization", method);
  requiredGraphStringParam(authorization, "session_scope_key", method);
  requiredGraphStringParam(authorization, "agent_scope_key", method);
  switch (method) {
    case "graph_extract_prepare":
      break;
    case "graph_upsert_candidates":
      requiredGraphStringParam(value, "extraction_run_id", method);
      const provider = requiredGraphObjectParam(value, "provider", method);
      for (const key of [
        "provider_id",
        "model_id",
        "extractor_version",
        "prompt_version",
        "schema_version",
      ]) {
        requiredGraphStringParam(provider, key, method);
      }
      break;
    case "graph_run_status":
      requiredGraphStringParam(value, "extraction_run_id", method);
      break;
    case "graph_extract_enqueue": {
      requiredGraphStringParam(value, "job_id", method);
      const provider = requiredGraphObjectParam(value, "provider", method);
      for (const key of [
        "provider_id",
        "model_id",
        "extractor_version",
        "prompt_version",
        "schema_version",
      ]) {
        requiredGraphStringParam(provider, key, method);
      }
      break;
    }
    case "graph_extract_claim":
      requiredGraphStringParam(value, "claim_request_id", method);
      requiredGraphStringParam(value, "worker_id", method);
      break;
    case "graph_extract_renew":
      requiredGraphStringParam(value, "job_id", method);
      requiredGraphStringParam(value, "lease_token", method);
      break;
    case "graph_extract_complete":
      requiredGraphStringParam(value, "job_id", method);
      requiredGraphStringParam(value, "lease_token", method);
      requiredGraphStringParam(value, "extraction_run_id", method);
      break;
    case "graph_extract_fail":
      requiredGraphStringParam(value, "job_id", method);
      requiredGraphStringParam(value, "lease_token", method);
      requiredGraphStringParam(value, "extraction_run_id", method);
      requiredGraphStringParam(value, "error_code", method);
      break;
    case "graph_extract_finish":
      requiredGraphStringParam(value, "job_id", method);
      requiredGraphStringParam(value, "lease_token", method);
      requiredGraphStringParam(value, "extraction_run_id", method);
      requiredGraphStringParam(value, "outcome", method);
      break;
    case "graph_extract_job_status":
      requiredGraphStringParam(value, "job_id", method);
      break;
    case "graph_extract_cancel":
      requiredGraphStringParam(value, "job_id", method);
      break;
    case "graph_observation_action":
      requiredGraphStringParam(value, "observation_id", method);
      requiredGraphStringParam(value, "action", method);
      break;
    case "graph_search":
      requiredGraphStringParam(value, "query", method);
      break;
    case "graph_status":
    case "graph_export":
      break;
  }
  return value;
}

function graphJsonValue(
  value: unknown,
  label: string,
  depth: number,
  ancestors: Set<object>,
): JsonValue {
  if (depth > MAX_VALUE_DEPTH) {
    throw new Error(`${label} nesting exceeds limit`);
  }
  if (value === null || typeof value === "boolean" || typeof value === "string") return value;
  if (typeof value === "bigint") {
    if (value < BigInt(Number.MIN_SAFE_INTEGER) || value > BigInt(Number.MAX_SAFE_INTEGER)) {
      throw new Error(`${label} contains an integer outside the JavaScript safe range`);
    }
    return value.toString();
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new Error(`${label} contains a non-finite number`);
    if (Number.isInteger(value) && !Number.isSafeInteger(value)) {
      throw new Error(`${label} contains an integer outside the JavaScript safe range`);
    }
    return value;
  }
  if (Array.isArray(value)) {
    if (ancestors.has(value)) throw new Error(`${label} contains a cyclic object`);
    const nextAncestors = new Set(ancestors).add(value);
    return value.map((item) => graphJsonValue(item, label, depth + 1, nextAncestors));
  }
  if (typeof value === "object") {
    if (ancestors.has(value)) throw new Error(`${label} contains a cyclic object`);
    const nextAncestors = new Set(ancestors).add(value);
    const object: JsonObject = {};
    for (const [key, item] of Object.entries(value)) {
      if (item !== undefined) {
        object[key] = graphJsonValue(item, `${label}.${key}`, depth + 1, nextAncestors);
      }
    }
    return object;
  }
  throw new Error(`${label} contains an unsupported value: ${typeof value}`);
}

function requiredGraphObjectParam(
  params: JsonObject,
  key: string,
  method: GraphMethod,
): JsonObject {
  const value = params[key];
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`Memory ${method} requires ${key} to be an object`);
  }
  return value;
}

function requiredGraphStringParam(params: JsonObject, key: string, method: GraphMethod): string {
  const value = params[key];
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`Memory ${method} requires a non-empty ${key}`);
  }
  return value;
}

function graphMessageObject(desc: DescMessage, message: Message): Record<string, unknown> {
  const reflected = reflect(desc, message);
  const result: Record<string, unknown> = {};
  for (const field of reflected.fields) {
    if (
      reflected.isSet(field) ||
      field.fieldKind === "list" ||
      field.fieldKind === "map" ||
      field.presence === FeatureSet_FieldPresence.IMPLICIT
    ) {
      result[field.name] = graphReflectValue(reflected.get(field), `graph ${field.name}`);
    }
  }
  return result;
}

function graphJobResponseObject(desc: DescMessage, message: Message): Record<string, unknown> {
  const result = graphMessageObject(desc, message);
  if (
    typeof result.outcome === "string" &&
    !["cancelled", "cancel_requested", "already_terminal"].includes(result.outcome)
  ) {
    throw new Error(`Native memory daemon returned unknown graph cancel outcome ${result.outcome}`);
  }
  const job = result.job;
  if (job === null || typeof job !== "object" || Array.isArray(job)) return result;
  const jobRecord = job as Record<string, unknown>;
  const state = jobRecord.state;
  if (typeof state !== "number") return result;
  return {
    ...result,
    job: {
      ...jobRecord,
      state: graphExtractionJobState(state),
    },
  };
}

function graphExtractionJobState(value: number): string {
  switch (value) {
    case 1:
      return "queued";
    case 2:
      return "claimed";
    case 3:
      return "running";
    case 4:
      return "completed";
    case 5:
      return "failed";
    case 6:
      return "cancelled";
    default:
      throw new Error(`Native memory daemon returned unknown graph job state ${value}`);
  }
}

function graphReflectValue(value: unknown, label: string): unknown {
  if (isReflectMessage(value)) return graphMessageObject(value.desc, value.message);
  if (isReflectList(value)) {
    return Array.from(value, (item) => graphReflectValue(item, label));
  }
  if (isReflectMap(value)) {
    return Object.fromEntries(
      Array.from(value, ([key, item]) => [key, graphReflectValue(item, label)]),
    );
  }
  if (typeof value === "bigint") return safeNumber(value, label);
  return value;
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
