import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import {
  Method,
  RequestSchema,
  ResponseSchema,
  ValueObjectSchema,
  ValueSchema,
} from "./generated/opencode/memory/v1/memory_pb.js";
import {
  DaemonRequestSchema,
  DaemonResponseSchema,
  DaemonStatusCode,
  CancelCallRequestSchema,
  GetDaemonInfoRequestSchema,
  ProjectCallRequestSchema,
} from "./generated/opencode/memory/daemon/v1/daemon_pb.js";
import {
  EmbeddingMetric,
  EmbeddingModality,
  ListModelProfilesResponseSchema,
  ModelPreflightDecision,
  ModelProfileCapability,
  ModelProfileRole,
  ModelProfileSchema,
  ModelProfileSupportLevel,
  ModelRequestSchema,
  ModelResponseSchema,
  ModelStatusCode,
  ModelStatusSchema,
  ModelSwitchAvailability,
  ModelSwitchExecutionMode,
  ModelSwitchPreflightSchema,
  ModelSwitchRebuildPolicy,
  ModelSwitchState,
  StartModelSwitchResponseSchema,
} from "./generated/opencode/memory/model/v1/model_pb.js";
import {
  createModelRequest,
  createProjectRequest,
  decodeModelResponse,
  decodeResponse,
  DelimitedFrameDecoder,
  encodeDelimited,
  encodeRequest,
} from "./protocol.js";
import type { ModelMethod } from "./protocol.js";

describe("Protobuf memory protocol", () => {
  test("encodes a typed request with length-delimited framing", () => {
    const frame = encodeRequest(7, "search", {
      query: "memory",
      retrieval_mode: "lexical",
      max_results: 5,
      enabled: true,
    });
    const [payload] = new DelimitedFrameDecoder(1024).push(frame);
    expect(payload).toBeDefined();
    const request = fromBinary(RequestSchema, payload!);
    expect(request.id).toBe(7n);
    expect(request.method).toBe(Method.SEARCH);
    expect(request.params?.kind.case).toBe("objectValue");
  });

  test("decodes fragmented response frames", () => {
    const result = create(ValueSchema, {
      kind: {
        case: "objectValue",
        value: create(ValueObjectSchema, {
          fields: {
            ready: create(ValueSchema, {
              kind: { case: "booleanValue", value: true },
            }),
            version: create(ValueSchema, {
              kind: { case: "unsignedValue", value: 2n },
            }),
          },
        }),
      },
    });
    const payload = toBinary(ResponseSchema, create(ResponseSchema, { id: 9n, ok: true, result }));
    const frame = withLength(payload);
    const decoder = new DelimitedFrameDecoder(1024);
    expect(decoder.push(frame.slice(0, 2))).toEqual([]);
    const [decodedPayload] = decoder.push(frame.slice(2));
    expect(decodeResponse(decodedPayload!)).toEqual({
      id: 9,
      ok: true,
      result: { ready: true, version: 2 },
      error: undefined,
    });
  });

  test("rejects unknown methods before writing to the native transport", () => {
    expect(() => encodeRequest(1, "unknown", {})).toThrow("Unknown memory method");
  });

  test("rejects integers outside the symmetric JavaScript safe range", () => {
    expect(() => encodeRequest(1, "status", { value: Number.MAX_SAFE_INTEGER })).not.toThrow();
    expect(() => encodeRequest(1, "status", { value: 2 ** 53 })).toThrow("safe range");
    expect(() =>
      encodeRequest(1, "status", { value: BigInt(Number.MAX_SAFE_INTEGER) + 1n }),
    ).toThrow("safe range");
    expect(() => encodeRequest(1, "status", { value: -(2n ** 63n) })).toThrow("safe range");
    expect(() => encodeRequest(1, "status", { value: 2n ** 64n - 1n })).toThrow("safe range");
  });

  test("encodes document ingestion as its own method", () => {
    const frame = encodeRequest(3, "ingest", { path: "paper.pdf" });
    const [payload] = new DelimitedFrameDecoder(1024).push(frame);
    const request = fromBinary(RequestSchema, payload!);
    expect(request.method).toBe(Method.INGEST);
  });

  test("encodes automatic document indexing as its own method", () => {
    const frame = encodeRequest(4, "index_documents", { force: false });
    const [payload] = new DelimitedFrameDecoder(1024).push(frame);
    const request = fromBinary(RequestSchema, payload!);
    expect(request.method).toBe(Method.INDEX_DOCUMENTS);
  });

  test("routes all model controls through typed operations", () => {
    type ModelOperation = "listProfiles" | "startSwitch" | "getSwitchStatus" | "cancelSwitch";
    const operations: Array<[ModelMethod, unknown, ModelOperation]> = [
      ["model_profiles", {}, "listProfiles"],
      [
        "model_switch",
        {
          switch_id: "switch-1",
          target_profile_id: "qwen3-text-0.6b-q8",
          expected_active_profile_id: "qwen3-text-4b-q4",
          allow_dense_downtime: true,
          dry_run: true,
          force_rebuild: true,
        },
        "startSwitch",
      ],
      ["model_switch_status", { switch_id: "switch-1" }, "getSwitchStatus"],
      ["model_switch_cancel", { switch_id: "switch-1" }, "cancelSwitch"],
    ];
    for (const [method, params, operation] of operations) {
      const request = createModelRequest(5, method, params);
      const decoded = fromBinary(ModelRequestSchema, toBinary(ModelRequestSchema, request));
      expect(decoded.id).toBe(5n);
      expect(decoded.operation.case).toBe(operation);
    }

    const start = createModelRequest(6, "model_switch", operations[1]![1]);
    expect(start.operation.case).toBe("startSwitch");
    if (start.operation.case !== "startSwitch") throw new Error("expected start switch");
    expect(start.operation.value).toMatchObject({
      switchId: "switch-1",
      targetProfileId: "qwen3-text-0.6b-q8",
      expectedActiveProfileId: "qwen3-text-4b-q4",
      availability: ModelSwitchAvailability.ALLOW_DENSE_DOWNTIME,
      executionMode: ModelSwitchExecutionMode.DRY_RUN,
      rebuildPolicy: ModelSwitchRebuildPolicy.FORCE_REBUILD,
    });
  });

  test("puts exactly one domain request branch on a project call", () => {
    const domain = createProjectRequest(7, "model_profiles", {});
    expect(domain.kind).toBe("model");
    if (domain.kind !== "model") throw new Error("expected model request");
    const projectCall = create(ProjectCallRequestSchema, {
      callId: "call-7",
      modelRequest: domain.modelRequest,
    });
    const decoded = fromBinary(
      ProjectCallRequestSchema,
      toBinary(ProjectCallRequestSchema, projectCall),
    );
    expect(decoded.request).toBeUndefined();
    expect(decoded.modelRequest?.operation.case).toBe("listProfiles");

    const memory = createProjectRequest(8, "status", {});
    expect(memory.kind).toBe("memory");
    if (memory.kind !== "memory") throw new Error("expected memory request");
    expect(memory.request.method).toBe(Method.STATUS);
  });

  test("maps typed model profiles to the existing snake_case contract", () => {
    const profile = create(ModelProfileSchema, {
      profileId: "qwen3-text-4b-q4",
      displayName: "Qwen3 4B",
      description: "Default profile",
      modalities: [EmbeddingModality.TEXT],
      repository: "Qwen/Qwen3-Embedding-4B-GGUF",
      runtimeFamily: "llama.cpp-gguf-text",
      dimension: 2560,
      metric: EmbeddingMetric.COSINE,
      supportLevel: ModelProfileSupportLevel.STABLE,
      roles: [ModelProfileRole.DEFAULT_FOR_NEW_PROJECTS, ModelProfileRole.RECOMMENDED],
      capabilities: [
        ModelProfileCapability.SELECTABLE,
        ModelProfileCapability.INSTALLED,
        ModelProfileCapability.PLATFORM_SUPPORTED,
        ModelProfileCapability.RUNTIME_AVAILABLE,
        ModelProfileCapability.ARTIFACT_LOCKED,
      ],
      estimatedDownloadBytes: 2_496_703_776n,
      estimatedResidentBytes: 8_000_000_000n,
    });
    const response = create(ModelResponseSchema, {
      id: 9n,
      status: create(ModelStatusSchema, { code: ModelStatusCode.OK }),
      result: {
        case: "listProfiles",
        value: create(ListModelProfilesResponseSchema, {
          catalogVersion: 1,
          catalogDigest: "digest",
          activeProfileId: profile.profileId,
          activeGenerationId: "legacy",
          profiles: [profile],
        }),
      },
    });

    expect(decodeModelResponse(response, "model_profiles")).toEqual({
      id: 9,
      ok: true,
      result: {
        catalog_version: 1,
        catalog_digest: "digest",
        active_profile_id: "qwen3-text-4b-q4",
        active_generation_id: "legacy",
        profiles: [
          {
            profile_id: "qwen3-text-4b-q4",
            display_name: "Qwen3 4B",
            description: "Default profile",
            modalities: ["text"],
            repository: "Qwen/Qwen3-Embedding-4B-GGUF",
            filename: null,
            revision: null,
            artifact_sha256: null,
            runtime_family: "llama.cpp-gguf-text",
            dimension: 2560,
            metric: "cosine",
            support_level: "stable",
            selectable: true,
            default_for_new_projects: true,
            recommended: true,
            installed: true,
            platform_supported: true,
            runtime_available: true,
            artifact_locked: true,
            estimated_download_bytes: 2_496_703_776,
            estimated_resident_bytes: 8_000_000_000,
            unavailable_reason: null,
          },
        ],
      },
      error: undefined,
    });
  });

  test("maps typed model switch preflight and safely rejects oversized uint64 values", () => {
    const preflight = create(ModelSwitchPreflightSchema, {
      decision: ModelPreflightDecision.BLOCKED,
      availability: ModelSwitchAvailability.ALLOW_DENSE_DOWNTIME,
      blockers: [{ code: "NOT_READY", message: "migration is disabled" }],
      warnings: ["dry run only"],
      estimatedDownloadBytes: 10n,
      estimatedDiskBytes: 20n,
      estimatedResidentBytes: 30n,
      denseSearchAvailable: true,
    });
    const response = create(ModelResponseSchema, {
      id: 10n,
      status: create(ModelStatusSchema, { code: ModelStatusCode.OK }),
      result: {
        case: "startSwitch",
        value: create(StartModelSwitchResponseSchema, {
          executionMode: ModelSwitchExecutionMode.DRY_RUN,
          state: ModelSwitchState.PREFLIGHT,
          activeProfileId: "qwen3-text-4b-q4",
          targetProfileId: "qwen3-text-0.6b-q8",
          activeGenerationId: "legacy",
          preflight,
          denseSearchAvailable: true,
        }),
      },
    });
    expect(decodeModelResponse(response, "model_switch").result).toEqual({
      switch_id: null,
      dry_run: true,
      state: "preflight",
      active_profile_id: "qwen3-text-4b-q4",
      target_profile_id: "qwen3-text-0.6b-q8",
      active_generation_id: "legacy",
      target_generation_id: null,
      dense_search_available: true,
      preflight: {
        can_start: false,
        availability: "allow_dense_downtime",
        dense_search_available: true,
        estimated_download_bytes: 10,
        estimated_disk_bytes: 20,
        estimated_resident_bytes: 30,
        warnings: ["dry run only"],
        blockers: [{ code: "NOT_READY", message: "migration is disabled" }],
      },
    });

    preflight.estimatedDownloadBytes = BigInt(Number.MAX_SAFE_INTEGER) + 1n;
    expect(() => decodeModelResponse(response, "model_switch")).toThrow("safe integer range");
  });

  test("rejects a typed model result that does not match the requested operation", () => {
    const response = create(ModelResponseSchema, {
      id: 11n,
      status: create(ModelStatusSchema, { code: ModelStatusCode.OK }),
      result: {
        case: "listProfiles",
        value: create(ListModelProfilesResponseSchema),
      },
    });
    expect(() => decodeModelResponse(response, "model_switch")).toThrow(
      "returned listProfiles result for model_switch",
    );
  });

  test("encodes a versioned daemon envelope with opaque request IDs", () => {
    const request = create(DaemonRequestSchema, {
      requestId: "call-2^53-plus-one",
      protocolGeneration: 1,
      body: { case: "getDaemonInfo", value: create(GetDaemonInfoRequestSchema) },
    });
    const frame = encodeDelimited(toBinary(DaemonRequestSchema, request));
    const [payload] = new DelimitedFrameDecoder(1024).push(frame);
    const decoded = fromBinary(DaemonRequestSchema, payload!);
    expect(decoded.requestId).toBe("call-2^53-plus-one");
    expect(decoded.body.case).toBe("getDaemonInfo");
  });

  test("keeps daemon status codes separate from domain responses", () => {
    const response = create(DaemonResponseSchema, {
      requestId: "request-1",
      status: { code: DaemonStatusCode.OUTCOME_UNKNOWN, message: "ambiguous" },
    });
    const decoded = fromBinary(DaemonResponseSchema, toBinary(DaemonResponseSchema, response));
    expect(decoded.status?.code).toBe(DaemonStatusCode.OUTCOME_UNKNOWN);
    expect(decoded.body.case).toBeUndefined();
  });

  test("encodes cancellation as a distinct daemon control operation", () => {
    const request = create(DaemonRequestSchema, {
      requestId: "cancel-request",
      protocolGeneration: 1,
      body: {
        case: "cancelCall",
        value: create(CancelCallRequestSchema, {
          sessionId: "session-a",
          projectHandle: "project-a",
          leaseId: "lease-a",
          callId: "call-a",
        }),
      },
    });
    const decoded = fromBinary(DaemonRequestSchema, toBinary(DaemonRequestSchema, request));
    expect(decoded.body.case).toBe("cancelCall");
  });
});

function withLength(payload: Uint8Array): Uint8Array {
  if (payload.byteLength >= 128) throw new Error("test payload is too large");
  const frame = new Uint8Array(payload.byteLength + 1);
  frame[0] = payload.byteLength;
  frame.set(payload, 1);
  return frame;
}
