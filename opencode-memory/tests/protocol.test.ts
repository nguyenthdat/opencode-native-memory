import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import {
  Method,
  RequestSchema,
  ResponseSchema,
  ValueObjectSchema,
  ValueSchema,
} from "../src/generated/opencode/memory/v1/memory_pb.js";
import {
  DaemonRequestSchema,
  DaemonResponseSchema,
  DaemonStatusCode,
  CancelCallRequestSchema,
  GetDaemonInfoRequestSchema,
  ProjectCallRequestSchema,
} from "../src/generated/opencode/memory/daemon/v1/daemon_pb.js";
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
} from "../src/generated/opencode/memory/model/v1/model_pb.js";
import {
  GraphExtractCancelResponseSchema,
  GraphExtractClaimResponseSchema,
  GraphExtractEnqueueResponseSchema,
  GraphExtractFinishOutcome,
  GraphExtractFinishResponseSchema,
  GraphExtractJobStatusResponseSchema,
  GraphExtractPrepareResponseSchema,
  GraphExtractRenewResponseSchema,
  GraphExtractionJobState,
  GraphExportResponseSchema,
  GraphOperationStatusSchema,
  GraphRequestSchema,
  GraphResponseSchema,
  GraphRunStatusResponseSchema,
  GraphSearchResponseSchema,
  GraphStatusCode,
  GraphStatusResponseSchema,
  GraphUpsertCandidatesResponseSchema,
} from "../src/generated/opencode/memory/graph/v1/graph_pb.js";
import {
  createGraphRequest,
  createModelRequest,
  createProjectRequest,
  decodeGraphResponse,
  decodeModelResponse,
  decodeResponse,
  DelimitedFrameDecoder,
  encodeDelimited,
  encodeRequest,
} from "../src/protocol.js";
import type { GraphMethod, ModelMethod } from "../src/protocol.js";
import type {
  GraphRequest,
  GraphResponse,
} from "../src/generated/opencode/memory/graph/v1/graph_pb.js";

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

  test("routes all graph methods through typed operations with snake_case parameters", () => {
    const authorization = {
      session_scope_key: "session-a",
      agent_scope_key: "agent-a",
    };
    const operations: Array<
      [GraphMethod, Record<string, unknown>, GraphRequest["operation"]["case"]]
    > = [
      [
        "graph_extract_prepare",
        { authorization, source_memory_ids: ["memory-1"], max_units: 4 },
        "extractPrepare",
      ],
      [
        "graph_upsert_candidates",
        {
          authorization,
          extraction_run_id: "run-1",
          sources: [
            {
              source_memory_id: "memory-1",
              source_unit_id: "unit-1",
              content_hash: "sha256:source",
              extraction_revision: "revision-1",
              derived_scope: {
                project_id: "project-1",
                memory_scope: "project",
                verified_scope_key: "scope-1",
              },
              policy_revision: "policy-1",
              remote_eligible: true,
            },
          ],
          provider: {
            provider_id: "opencode",
            model_id: "model-1",
            extractor_version: "extractor-1",
            prompt_version: "prompt-1",
            schema_version: "graph-1",
          },
          entities: [
            {
              mention: "zvec",
              entity_type: "technology",
              evidence: [{ source_unit_id: "unit-1", quote: "zvec" }],
            },
          ],
          relations: [],
        },
        "upsertCandidates",
      ],
      ["graph_run_status", { authorization, extraction_run_id: "run-1" }, "runStatus"],
      [
        "graph_extract_enqueue",
        {
          authorization,
          job_id: "job-1",
          source_memory_ids: ["memory-1"],
          provider: {
            provider_id: "opencode",
            model_id: "model-1",
            extractor_version: "extractor-1",
            prompt_version: "prompt-1",
            schema_version: "graph-1",
          },
          max_attempts: 3,
        },
        "extractEnqueue",
      ],
      [
        "graph_extract_claim",
        { authorization, claim_request_id: "claim-1", worker_id: "worker-1" },
        "extractClaim",
      ],
      [
        "graph_extract_renew",
        { authorization, job_id: "job-1", lease_token: "a".repeat(64) },
        "extractRenew",
      ],
      [
        "graph_extract_complete",
        {
          authorization,
          job_id: "job-1",
          lease_token: "a".repeat(64),
          extraction_run_id: "run-1",
          entities: [],
          relations: [],
        },
        "extractFinish",
      ],
      [
        "graph_extract_fail",
        {
          authorization,
          job_id: "job-1",
          lease_token: "a".repeat(64),
          extraction_run_id: "run-1",
          retryable: false,
          error_code: "provider_error",
          error_message: "provider failed",
        },
        "extractFinish",
      ],
      [
        "graph_extract_finish",
        {
          authorization,
          job_id: "job-1",
          lease_token: "a".repeat(64),
          extraction_run_id: "run-1",
          outcome: "GRAPH_EXTRACT_FINISH_OUTCOME_RETRYABLE_FAILURE",
          error_code: "provider_error",
        },
        "extractFinish",
      ],
      ["graph_extract_job_status", { authorization, job_id: "job-1" }, "extractJobStatus"],
      ["graph_extract_cancel", { authorization, job_id: "job-1" }, "extractCancel"],
      [
        "graph_observation_action",
        { authorization, observation_id: "obs_1", action: "invalidate" },
        "observationAction",
      ],
      [
        "graph_search",
        {
          authorization,
          query: "vector database",
          time: { valid_after_ms: 10 },
          max_depth: 2,
          max_fanout: 32,
          max_results: 64,
        },
        "search",
      ],
      ["graph_status", { authorization, scope: { memory_scope: "project" } }, "status"],
      ["graph_export", { authorization, cursor: "cursor-1", page_limit: 32 }, "export"],
    ];

    for (const [method, params, operation] of operations) {
      const request = createGraphRequest(20, method, params);
      const decoded = fromBinary(GraphRequestSchema, toBinary(GraphRequestSchema, request));
      expect(decoded.id).toBe(20n);
      expect(decoded.operation.case).toBe(operation);
    }

    const upsert = createGraphRequest(21, "graph_upsert_candidates", operations[1]![1]);
    if (upsert.operation.case !== "upsertCandidates") throw new Error("expected graph upsert");
    expect(upsert.operation.value.provider?.providerId).toBe("opencode");
    expect(upsert.operation.value.sources[0]?.sourceMemoryId).toBe("memory-1");
    expect(upsert.operation.value.entities[0]?.evidence[0]?.sourceUnitId).toBe("unit-1");

    const completed = createGraphRequest(22, "graph_extract_complete", operations[6]![1]);
    expect(completed.operation.case).toBe("extractFinish");
    if (completed.operation.case !== "extractFinish") throw new Error("expected graph finish");
    expect(completed.operation.value.outcome).toBe(GraphExtractFinishOutcome.COMPLETED);
    const failed = createGraphRequest(23, "graph_extract_fail", operations[7]![1]);
    if (failed.operation.case !== "extractFinish") throw new Error("expected graph failure");
    expect(failed.operation.value.outcome).toBe(GraphExtractFinishOutcome.PERMANENT_FAILURE);
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

  test("puts a graph request on the graph ProjectCall branch", () => {
    const graph = createProjectRequest(22, "graph_status", {
      authorization: { session_scope_key: "session-a", agent_scope_key: "agent-a" },
    });
    expect(graph.kind).toBe("graph");
    if (graph.kind !== "graph") throw new Error("expected graph request");
    const projectCall = create(ProjectCallRequestSchema, {
      callId: "call-graph",
      graphRequest: graph.graphRequest,
    });
    const decoded = fromBinary(
      ProjectCallRequestSchema,
      toBinary(ProjectCallRequestSchema, projectCall),
    );
    expect(decoded.request).toBeUndefined();
    expect(decoded.modelRequest).toBeUndefined();
    expect(decoded.graphRequest?.operation.case).toBe("status");
  });

  test("decodes all graph result branches into snake_case unknown objects", () => {
    const status = create(GraphOperationStatusSchema, { code: GraphStatusCode.OK });
    const receipt = {
      extractionRunId: "run-1",
      idempotencyDigest: "digest-1",
      outcome: "committed",
      committedAtMs: 100n,
      sourceCount: 2n,
      acceptedEntityCount: 1n,
      acceptedRelationCount: 1n,
      rejectedCandidateCount: 1n,
      conflictCount: 0n,
      warningCount: 1n,
      terminal: true,
    };
    const job = {
      jobId: "job-1",
      idempotencyDigest: "job-digest",
      state: GraphExtractionJobState.RUNNING,
      attemptCount: 1,
      maxAttempts: 3,
      createdAtMs: 90n,
      updatedAtMs: 100n,
      leaseExpiresAtMs: 60_100n,
      extractionRunId: "run-1",
      cancelRequested: false,
      maxUnitTextBytes: 32_768,
      maxTotalTextBytes: 262_144,
    };
    const responses: Array<[GraphMethod, GraphResponse, Record<string, unknown>]> = [
      [
        "graph_extract_prepare",
        create(GraphResponseSchema, {
          id: 30n,
          status,
          result: {
            case: "extractPrepare",
            value: create(GraphExtractPrepareResponseSchema, {
              requestedSourceCount: 2n,
              warnings: ["one source was redacted"],
            }),
          },
        }),
        { requested_source_count: 2, warnings: ["one source was redacted"] },
      ],
      [
        "graph_upsert_candidates",
        create(GraphResponseSchema, {
          id: 30n,
          status,
          result: {
            case: "upsertCandidates",
            value: create(GraphUpsertCandidatesResponseSchema, {
              receipt,
              acceptedEntities: [
                {
                  candidateIndex: 1,
                  entityId: "entity-1",
                  canonicalName: "zvec",
                  entityType: "technology",
                },
              ],
              rejectedCandidates: [
                { candidateKind: "relation", candidateIndex: 1, code: "NO_EVIDENCE" },
              ],
              warnings: ["bounded"],
            }),
          },
        }),
        {
          receipt: {
            extraction_run_id: "run-1",
            idempotency_digest: "digest-1",
            outcome: "committed",
            committed_at_ms: 100,
            source_count: 2,
            accepted_entity_count: 1,
            accepted_relation_count: 1,
            rejected_candidate_count: 1,
            warning_count: 1,
            terminal: true,
          },
          accepted_entities: [
            {
              candidate_index: 1,
              entity_id: "entity-1",
              canonical_name: "zvec",
              entity_type: "technology",
            },
          ],
          rejected_candidates: [
            { candidate_kind: "relation", candidate_index: 1, code: "NO_EVIDENCE" },
          ],
          warnings: ["bounded"],
        },
      ],
      [
        "graph_run_status",
        create(GraphResponseSchema, {
          id: 30n,
          status,
          result: {
            case: "runStatus",
            value: create(GraphRunStatusResponseSchema, { found: true, receipt }),
          },
        }),
        { found: true, receipt: { extraction_run_id: "run-1", committed_at_ms: 100 } },
      ],
      [
        "graph_search",
        create(GraphResponseSchema, {
          id: 30n,
          status,
          result: {
            case: "search",
            value: create(GraphSearchResponseSchema, {
              eligibleSourceCount: 3n,
              truncated: true,
            }),
          },
        }),
        { eligible_source_count: 3, truncated: true },
      ],
      [
        "graph_status",
        create(GraphResponseSchema, {
          id: 30n,
          status,
          result: {
            case: "graphStatus",
            value: create(GraphStatusResponseSchema, {
              schemaVersion: "graph-1",
              entityCount: 4n,
              relationCount: 5n,
              pendingJobCount: 1n,
              lastExtraction: { extractionRunId: "run-1", completedAtMs: 100n, sourceCount: 2n },
            }),
          },
        }),
        {
          schema_version: "graph-1",
          entity_count: 4,
          relation_count: 5,
          pending_job_count: 1,
          last_extraction: {
            extraction_run_id: "run-1",
            completed_at_ms: 100,
            source_count: 2,
          },
        },
      ],
      [
        "graph_export",
        create(GraphResponseSchema, {
          id: 30n,
          status,
          result: {
            case: "export",
            value: create(GraphExportResponseSchema, {
              schemaVersion: "graph-1",
              nextCursor: "cursor-2",
              complete: false,
            }),
          },
        }),
        { schema_version: "graph-1", next_cursor: "cursor-2" },
      ],
      [
        "graph_extract_enqueue",
        create(GraphResponseSchema, {
          id: 30n,
          status,
          result: {
            case: "extractEnqueue",
            value: create(GraphExtractEnqueueResponseSchema, { job, existing: false }),
          },
        }),
        { job: { job_id: "job-1", state: "running" }, existing: false },
      ],
      [
        "graph_extract_claim",
        create(GraphResponseSchema, {
          id: 30n,
          status,
          result: {
            case: "extractClaim",
            value: create(GraphExtractClaimResponseSchema, {
              found: true,
              job,
              leaseToken: "a".repeat(64),
            }),
          },
        }),
        { found: true, lease_token: "a".repeat(64), job: { job_id: "job-1" } },
      ],
      [
        "graph_extract_renew",
        create(GraphResponseSchema, {
          id: 30n,
          status,
          result: {
            case: "extractRenew",
            value: create(GraphExtractRenewResponseSchema, {
              job,
              leaseExpiresAtMs: 60_100n,
            }),
          },
        }),
        { lease_expires_at_ms: 60_100, job: { state: "running" } },
      ],
      [
        "graph_extract_complete",
        create(GraphResponseSchema, {
          id: 30n,
          status,
          result: {
            case: "extractFinish",
            value: create(GraphExtractFinishResponseSchema, { job }),
          },
        }),
        { job: { job_id: "job-1" } },
      ],
      [
        "graph_extract_job_status",
        create(GraphResponseSchema, {
          id: 30n,
          status,
          result: {
            case: "extractJobStatus",
            value: create(GraphExtractJobStatusResponseSchema, { found: true, job }),
          },
        }),
        { found: true, job: { updated_at_ms: 100 } },
      ],
      [
        "graph_extract_cancel",
        create(GraphResponseSchema, {
          id: 30n,
          status,
          result: {
            case: "extractCancel",
            value: create(GraphExtractCancelResponseSchema, {
              job,
              outcome: "cancel_requested",
            }),
          },
        }),
        { outcome: "cancel_requested", job: { job_id: "job-1" } },
      ],
    ];

    for (const [method, response, result] of responses) {
      expect(decodeGraphResponse(response, method)).toMatchObject({
        id: 30,
        ok: true,
        result,
        error: undefined,
      });
    }
  });

  test("rejects a graph result that does not match the requested operation", () => {
    const response = create(GraphResponseSchema, {
      id: 31n,
      status: create(GraphOperationStatusSchema, { code: GraphStatusCode.OK }),
      result: {
        case: "extractPrepare",
        value: create(GraphExtractPrepareResponseSchema),
      },
    });
    expect(() => decodeGraphResponse(response, "graph_status")).toThrow(
      "returned extractPrepare result for graph_status",
    );
  });

  test("rejects unknown durable graph job states and cancel outcomes", () => {
    const status = create(GraphOperationStatusSchema, { code: GraphStatusCode.OK });
    const unknownState = create(GraphResponseSchema, {
      id: 35n,
      status,
      result: {
        case: "extractJobStatus",
        value: create(GraphExtractJobStatusResponseSchema, {
          found: true,
          job: { jobId: "job-1", state: GraphExtractionJobState.UNSPECIFIED },
        }),
      },
    });
    expect(() => decodeGraphResponse(unknownState, "graph_extract_job_status")).toThrow(
      "unknown graph job state",
    );

    const unknownCancel = create(GraphResponseSchema, {
      id: 36n,
      status,
      result: {
        case: "extractCancel",
        value: create(GraphExtractCancelResponseSchema, {
          job: { jobId: "job-1", state: GraphExtractionJobState.CANCELLED },
          outcome: "future_outcome",
        }),
      },
    });
    expect(() => decodeGraphResponse(unknownCancel, "graph_extract_cancel")).toThrow(
      "unknown graph cancel outcome",
    );
  });

  test("preserves default-valued graph fields in public results", () => {
    const response = create(GraphResponseSchema, {
      id: 33n,
      status: create(GraphOperationStatusSchema, { code: GraphStatusCode.OK }),
      result: {
        case: "runStatus",
        value: create(GraphRunStatusResponseSchema, { found: false }),
      },
    });
    expect(decodeGraphResponse(response, "graph_run_status")).toEqual({
      id: 33,
      ok: true,
      result: { found: false },
      error: undefined,
    });

    const exportResponse = create(GraphResponseSchema, {
      id: 34n,
      status: create(GraphOperationStatusSchema, { code: GraphStatusCode.OK }),
      result: {
        case: "export",
        value: create(GraphExportResponseSchema),
      },
    });
    expect(decodeGraphResponse(exportResponse, "graph_export").result).toEqual({
      schema_version: "",
      entities: [],
      relations: [],
      facts: [],
      observations: [],
      provenance: [],
      complete: false,
    });
  });

  test("rejects graph uint64 values outside the JavaScript safe range", () => {
    const response = create(GraphResponseSchema, {
      id: 32n,
      status: create(GraphOperationStatusSchema, { code: GraphStatusCode.OK }),
      result: {
        case: "graphStatus",
        value: create(GraphStatusResponseSchema, {
          entityCount: BigInt(Number.MAX_SAFE_INTEGER) + 1n,
        }),
      },
    });
    expect(() => decodeGraphResponse(response, "graph_status")).toThrow("safe integer range");
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
