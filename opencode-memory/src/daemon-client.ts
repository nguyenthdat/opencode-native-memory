import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { existsSync, readFileSync, realpathSync } from "node:fs";
import {
  chmod,
  lstat,
  mkdir,
  open,
  readFile,
  stat,
  unlink,
  type FileHandle,
} from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createConnection, type Socket } from "node:net";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  AcquireProjectRequestSchema,
  CancelCallRequestSchema,
  CancelOutcome,
  DaemonRequestSchema,
  DaemonResponseSchema,
  DaemonStatusCode,
  DrainOutcome,
  EmbeddingIdentitySchema,
  GetDaemonInfoRequestSchema,
  OpenSessionRequestSchema,
  ProjectCallRequestSchema,
  ReleaseProjectRequestSchema,
  RequestDrainRequestSchema,
  SessionHeartbeatRequestSchema,
} from "./generated/opencode/memory/daemon/v1/daemon_pb.js";
import type {
  AcquireProjectResponse,
  DaemonRequest as DaemonRequestMessage,
  DaemonResponse,
  GetDaemonInfoResponse,
  OpenSessionResponse,
} from "./generated/opencode/memory/daemon/v1/daemon_pb.js";
import type { Response as MemoryResponse } from "./generated/opencode/memory/v1/memory_pb.js";
import type { ModelResponse } from "./generated/opencode/memory/model/v1/model_pb.js";
import type { GraphResponse } from "./generated/opencode/memory/graph/v1/graph_pb.js";
import {
  createProjectRequest,
  decodeMemoryResponse,
  decodeGraphResponse,
  decodeModelResponse,
  DelimitedFrameDecoder,
  encodeDelimited,
} from "./protocol.js";
import type { GraphMethod, MemoryMethod, ModelMethod } from "./protocol.js";

const MiB = 1024 * 1024;
const DEFAULT_REQUEST_TIMEOUT_MS = 300_000;
const MAX_REQUEST_TIMEOUT_MS = 2 * 60 * 60_000;
const MIN_REQUEST_TIMEOUT_MS = 1_000;
const CONNECT_TIMEOUT_MS = 5_000;
const STARTUP_TIMEOUT_MS = 15_000;
const START_LOCK_STALE_MS = 30_000;
const START_LOCK_TIMEOUT_MS = START_LOCK_STALE_MS + STARTUP_TIMEOUT_MS;
const DAEMON_PROTOCOL_GENERATION = 1;
const DOMAIN_SCHEMA_GENERATION = 6;
const require = createRequire(import.meta.url);
const NATIVE_PACKAGES: Partial<Record<string, string>> = {
  "darwin-arm64": "@nguyenthdat/opencode-memory-darwin-arm64",
  "linux-arm64": "@nguyenthdat/opencode-memory-linux-arm64-gnu",
  "linux-x64": "@nguyenthdat/opencode-memory-linux-x64-gnu",
};

export const MAX_REQUEST_BYTES = 32 * MiB;
export const MAX_RESPONSE_BYTES = 32 * MiB;
export const REQUEST_TIMEOUT_MS = configuredRequestTimeoutMs();
export const INITIALIZATION_TIMEOUT_MS = Math.max(REQUEST_TIMEOUT_MS, 30 * 60_000);

const RETRY_SAFE_METHODS = new Set<MemoryMethod>([
  "get",
  "list",
  "status",
  "doctor",
  "export",
  "model_profiles",
  "model_switch_status",
  "graph_extract_prepare",
  "graph_extract_enqueue",
  "graph_extract_claim",
  "graph_extract_renew",
  "graph_extract_job_status",
  "graph_extract_cancel",
  "graph_observation_action",
  "graph_run_status",
  "graph_search",
  "graph_status",
  "graph_export",
]);
const OUTCOME_RECONCILABLE_METHODS = new Set<MemoryMethod>([
  "model_switch_cancel",
  "graph_extract_enqueue",
  "graph_extract_claim",
  "graph_extract_renew",
  "graph_extract_cancel",
]);

export function isRetrySafeMemoryMethod(method: MemoryMethod): boolean {
  return RETRY_SAFE_METHODS.has(method);
}

export interface NativeMemoryRequester {
  request<T>(method: MemoryMethod, params?: unknown, signal?: AbortSignal): Promise<T>;
}

interface PendingDaemonRequest {
  resolve(response: DaemonResponse): void;
  reject(error: Error): void;
  timer: ReturnType<typeof setTimeout>;
  generation: number;
}

interface ReadyProject {
  daemon: GetDaemonInfoResponse;
  session: OpenSessionResponse;
  project: AcquireProjectResponse;
  generation: number;
}

export interface DaemonClientInfo {
  readonly endpoint: string;
  readonly daemonInstanceId: string;
  readonly daemonVersion: string;
  readonly pid: number;
  readonly sessionId: string;
  readonly projectHandle: string;
  readonly canonicalProjectId: string;
  readonly storeKeyHash: string;
  readonly capabilities: readonly string[];
}

export type DaemonControlInfo = Pick<
  GetDaemonInfoResponse,
  | "daemonInstanceId"
  | "daemonVersion"
  | "minimumProtocolGeneration"
  | "maximumProtocolGeneration"
  | "domainSchemaGeneration"
  | "capabilities"
  | "pid"
>;

export type DaemonDrainOutcome = "accepted" | "busy" | "unsupported";

export interface DaemonDrainResult {
  readonly daemon: DaemonControlInfo;
  readonly outcome: DaemonDrainOutcome;
  readonly retryAfterMs: number;
}

export class DaemonRpcError extends Error {
  readonly name = "DaemonRpcError";

  constructor(
    message: string,
    readonly code: DaemonStatusCode,
    readonly retryAfterMs = 0,
  ) {
    super(message);
  }
}

export class DaemonOutcomeUnknownError extends Error {
  readonly name = "DaemonOutcomeUnknownError";
  readonly code = "OUTCOME_UNKNOWN";

  constructor(
    message: string,
    readonly callId: string,
    options?: ErrorOptions,
  ) {
    super(message, options);
  }
}

class DaemonTransportError extends Error {
  readonly name = "DaemonTransportError";
}

class NativeMemoryOperationError extends Error {
  readonly name = "NativeMemoryOperationError";
}

class DaemonProjectClient implements NativeMemoryRequester {
  private socket: Socket | undefined;
  private generation = 0;
  private ready: ReadyProject | undefined;
  private connecting: Promise<ReadyProject> | undefined;
  private disposed = false;
  private disposePromise: Promise<void> | undefined;
  private readonly lifecycle = new AbortController();
  private activeRequests = 0;
  private activeRequestWaiters: Array<() => void> = [];
  private nextMemoryId = 1;
  private pending = new Map<string, PendingDaemonRequest>();
  private heartbeat: ReturnType<typeof setInterval> | undefined;
  private heartbeatInFlight = false;
  private readonly pluginVersion: string;
  readonly endpoint = resolveDaemonEndpoint();

  constructor(
    private readonly root: string,
    private readonly worktree: string,
    private readonly requestTimeoutMs: number,
  ) {
    this.pluginVersion = resolvePluginVersion(root);
  }

  async request<T>(method: MemoryMethod, params: unknown = {}, signal?: AbortSignal): Promise<T> {
    if (this.disposed) throw new Error("Native memory client is disposed");
    if (method === "shutdown") {
      throw new Error("Shared native memory clients cannot shut down the user daemon");
    }
    if (signal?.aborted) throw new Error("Native memory request was cancelled");
    const requestSignal = signal
      ? AbortSignal.any([signal, this.lifecycle.signal])
      : this.lifecycle.signal;
    this.activeRequests += 1;

    try {
      const callId = randomUUID();
      const isRetrySafe = isRetrySafeMemoryMethod(method);
      for (let attempt = 0; attempt < 2; attempt += 1) {
        let dispatched = false;
        let ready: ReadyProject | undefined;
        try {
          ready = await this.ensureProject(requestSignal);
          if (this.disposed || requestSignal.aborted) {
            throw new DaemonRpcError(
              "Native memory request was cancelled",
              DaemonStatusCode.CANCELLED,
            );
          }
          const requestId = this.nextMemoryId++;
          const projectRequest = createProjectRequest(requestId, method, params);
          const body = create(ProjectCallRequestSchema, {
            daemonInstanceId: ready.daemon.daemonInstanceId,
            sessionId: ready.session.sessionId,
            projectHandle: ready.project.projectHandle,
            leaseId: ready.project.leaseId,
            callId,
            timeoutMs: this.requestTimeoutMs,
            ...(projectRequest.kind === "memory"
              ? { request: projectRequest.request }
              : projectRequest.kind === "model"
                ? { modelRequest: projectRequest.modelRequest }
                : { graphRequest: projectRequest.graphRequest }),
          });
          const pending = this.send(
            { case: "projectCall", value: body },
            this.requestTimeoutMs,
            requestSignal,
          );
          dispatched = true;
          const response = await pending;
          if (response.body.case !== "projectCall") {
            throw new Error("Native memory daemon returned the wrong project call response");
          }
          if (response.body.value.callId !== callId) {
            throw new Error("Native memory daemon returned a mismatched call ID");
          }
          const memoryResponse = response.body.value.response;
          const modelResponse = response.body.value.modelResponse;
          const graphResponse = response.body.value.graphResponse;
          if ([memoryResponse, modelResponse, graphResponse].filter(Boolean).length !== 1) {
            throw new Error(
              "Native memory daemon project response must contain exactly one memory, model, or graph response",
            );
          }
          if (projectRequest.kind === "memory") {
            if (!memoryResponse) {
              throw new Error("Native memory daemon returned the wrong domain response branch");
            }
            if (memoryResponse.id !== BigInt(requestId)) {
              throw new Error("Native memory daemon returned a mismatched domain response ID");
            }
            return this.unwrapMemoryResponse<T>(memoryResponse);
          }
          if (projectRequest.kind === "model") {
            if (!modelResponse) {
              throw new Error("Native memory daemon returned the wrong domain response branch");
            }
            if (modelResponse.id !== BigInt(requestId)) {
              throw new Error("Native memory daemon returned a mismatched model response ID");
            }
            return this.unwrapModelResponse<T>(modelResponse, projectRequest.method);
          }
          if (!graphResponse) {
            throw new Error("Native memory daemon returned the wrong domain response branch");
          }
          if (graphResponse.id !== BigInt(requestId)) {
            throw new Error("Native memory daemon returned a mismatched graph response ID");
          }
          return this.unwrapGraphResponse<T>(graphResponse, projectRequest.method);
        } catch (error) {
          const failure = asError(error);
          const retryableTransport = failure instanceof DaemonTransportError;
          let rejectedBeforeAdmission = isDefinitePreAdmissionFailure(failure);
          if (
            dispatched &&
            ready &&
            failure instanceof DaemonRpcError &&
            [DaemonStatusCode.CANCELLED, DaemonStatusCode.DEADLINE_EXCEEDED].includes(failure.code)
          ) {
            const cancelOutcome = await this.cancelProjectCall(ready, callId).catch(
              () => undefined,
            );
            rejectedBeforeAdmission = cancelOutcome === CancelOutcome.CANCELLED_BEFORE_START;
          }
          if (
            ready &&
            failure instanceof DaemonRpcError &&
            [DaemonStatusCode.INTERNAL, DaemonStatusCode.OUTCOME_UNKNOWN].includes(failure.code)
          ) {
            this.invalidateConnection(ready.generation);
          }
          const reconnectRequired = isReconnectRequiredFailure(failure);
          const shouldReconnect = retryableTransport || reconnectRequired;
          if (shouldReconnect) this.invalidateConnection(ready?.generation ?? this.generation);
          if (
            attempt === 0 &&
            !requestSignal.aborted &&
            (retryableTransport
              ? isRetrySafe || !dispatched
              : reconnectRequired || (!dispatched && isRetryableSetupFailure(failure)))
          ) {
            continue;
          }
          const ambiguousMutation =
            !rejectedBeforeAdmission &&
            (isAmbiguousFailure(failure) || !(failure instanceof NativeMemoryOperationError));
          if (
            dispatched &&
            ambiguousMutation &&
            (!isRetrySafe || OUTCOME_RECONCILABLE_METHODS.has(method))
          ) {
            throw new DaemonOutcomeUnknownError(
              `Native memory ${method} may have committed before its response was lost (call_id=${callId})`,
              callId,
              { cause: failure },
            );
          }
          throw failure;
        }
      }
      throw new Error("Native memory daemon reconnect retry limit was exhausted");
    } finally {
      this.activeRequests -= 1;
      if (this.activeRequests === 0) {
        for (const resolveWaiter of this.activeRequestWaiters.splice(0)) resolveWaiter();
      }
    }
  }

  async info(): Promise<DaemonClientInfo> {
    const ready = await this.ensureProject();
    return {
      endpoint: this.endpoint,
      daemonInstanceId: ready.daemon.daemonInstanceId,
      daemonVersion: ready.daemon.daemonVersion,
      pid: ready.daemon.pid,
      sessionId: ready.session.sessionId,
      projectHandle: ready.project.projectHandle,
      canonicalProjectId: ready.project.canonicalProjectId,
      storeKeyHash: ready.project.storeKeyHash,
      capabilities: [...ready.daemon.capabilities],
    };
  }

  async probe(): Promise<DaemonControlInfo> {
    try {
      await this.connectTransport();
      const response = await this.send(
        { case: "getDaemonInfo", value: create(GetDaemonInfoRequestSchema) },
        CONNECT_TIMEOUT_MS,
      );
      if (response.body.case !== "getDaemonInfo") {
        throw new Error("Native memory daemon omitted GetDaemonInfo");
      }
      assertDaemonSchemaCompatible(this.pluginVersion, response.body.value, this.endpoint);
      const hello = create(OpenSessionRequestSchema, {
        clientInstanceId: randomUUID(),
        minimumProtocolGeneration: DAEMON_PROTOCOL_GENERATION,
        maximumProtocolGeneration: DAEMON_PROTOCOL_GENERATION,
        domainSchemaGeneration: DOMAIN_SCHEMA_GENERATION,
        pluginVersion: this.pluginVersion,
      });
      const session = await this.send({ case: "openSession", value: hello }, CONNECT_TIMEOUT_MS);
      if (session.body.case !== "openSession") {
        throw new Error("Native memory daemon omitted OpenSession during control probe");
      }
      return response.body.value;
    } finally {
      await this.dispose();
    }
  }

  async requestDrainIfRunning(): Promise<DaemonDrainResult | undefined> {
    try {
      try {
        await this.connectTransport(false);
      } catch (error) {
        if (isUnavailableEndpointError(error)) return undefined;
        throw error;
      }
      const infoResponse = await this.send(
        { case: "getDaemonInfo", value: create(GetDaemonInfoRequestSchema) },
        CONNECT_TIMEOUT_MS,
      );
      if (infoResponse.body.case !== "getDaemonInfo") {
        throw new Error("Native memory daemon omitted GetDaemonInfo before drain");
      }
      const daemon = infoResponse.body.value;
      // Drain must remain available when the project-domain schema is stale.
      const request = create(RequestDrainRequestSchema, {
        expectedDaemonInstanceId: daemon.daemonInstanceId,
      });
      const response = await this.send(
        { case: "requestDrain", value: request },
        CONNECT_TIMEOUT_MS,
      );
      if (response.body.case !== "requestDrain") {
        throw new Error("Native memory daemon omitted RequestDrain");
      }
      return {
        daemon,
        outcome: daemonDrainOutcome(response.body.value.outcome),
        retryAfterMs: response.body.value.retryAfterMs,
      };
    } finally {
      await this.dispose();
    }
  }

  async dispose(): Promise<void> {
    if (this.disposePromise) return await this.disposePromise;
    this.disposePromise = this.disposeOnce();
    return await this.disposePromise;
  }

  private async disposeOnce(): Promise<void> {
    this.disposed = true;
    this.lifecycle.abort();
    this.stopHeartbeat();
    if (this.activeRequests > 0) {
      await new Promise<void>((resolveWaiter) => this.activeRequestWaiters.push(resolveWaiter));
    }
    const ready = this.ready;
    if (ready && this.socket && !this.socket.destroyed) {
      const release = create(ReleaseProjectRequestSchema, {
        daemonInstanceId: ready.daemon.daemonInstanceId,
        sessionId: ready.session.sessionId,
        projectHandle: ready.project.projectHandle,
        leaseId: ready.project.leaseId,
      });
      try {
        await this.send({ case: "releaseProject", value: release }, CONNECT_TIMEOUT_MS);
      } catch {
        // Closing the session connection also releases every remaining lease.
      }
    }
    this.ready = undefined;
    this.connecting = undefined;
    const socket = this.socket;
    this.socket = undefined;
    if (socket && !socket.destroyed) {
      socket.end();
      socket.destroy();
    }
    this.rejectPending(new DaemonTransportError("Native memory daemon connection closed"));
  }

  private async ensureProject(signal?: AbortSignal): Promise<ReadyProject> {
    if (this.disposed) throw new Error("Native memory client is disposed");
    if (this.ready && this.socket && !this.socket.destroyed) return this.ready;
    if (!this.connecting) {
      const connecting = this.connectAndAcquire();
      this.connecting = connecting;
      void connecting
        .catch(() => {
          if (this.connecting === connecting) this.invalidateConnection(this.generation);
        })
        .finally(() => {
          if (this.connecting === connecting) this.connecting = undefined;
        });
    }
    const connecting = this.connecting!;
    const ready = await waitForPromise(connecting, signal);
    if (this.disposed) {
      this.invalidateConnection(ready.generation);
      throw new Error("Native memory client is disposed");
    }
    if (ready.generation !== this.generation || !this.socket || this.socket.destroyed) {
      throw new DaemonTransportError("Native memory daemon setup was superseded");
    }
    this.ready = ready;
    return ready;
  }

  private async connectAndAcquire(): Promise<ReadyProject> {
    await this.connectTransport();
    const generation = this.generation;

    const infoResponse = await this.send(
      { case: "getDaemonInfo", value: create(GetDaemonInfoRequestSchema) },
      CONNECT_TIMEOUT_MS,
    );
    if (infoResponse.body.case !== "getDaemonInfo") {
      throw new Error("Native memory daemon omitted GetDaemonInfo");
    }
    const daemon = infoResponse.body.value;
    if (
      daemon.minimumProtocolGeneration > DAEMON_PROTOCOL_GENERATION ||
      daemon.maximumProtocolGeneration < DAEMON_PROTOCOL_GENERATION
    ) {
      throw new Error(
        `Native memory daemon protocol mismatch at ${this.endpoint}: ` +
          `client supports ${DAEMON_PROTOCOL_GENERATION}, daemon ${daemon.daemonVersion} supports ` +
          `${daemon.minimumProtocolGeneration}-${daemon.maximumProtocolGeneration} (pid ${daemon.pid}). ` +
          "Close all OpenCode processes using memory and restart the native memory daemon.",
      );
    }
    assertDaemonSchemaCompatible(this.pluginVersion, daemon, this.endpoint);
    assertDaemonVersionCompatible(this.pluginVersion, daemon.daemonVersion, daemon.pid);
    const hello = create(OpenSessionRequestSchema, {
      clientInstanceId: randomUUID(),
      minimumProtocolGeneration: DAEMON_PROTOCOL_GENERATION,
      maximumProtocolGeneration: DAEMON_PROTOCOL_GENERATION,
      domainSchemaGeneration: DOMAIN_SCHEMA_GENERATION,
      pluginVersion: this.pluginVersion,
    });
    const sessionResponse = await this.send(
      { case: "openSession", value: hello },
      CONNECT_TIMEOUT_MS,
    );
    if (sessionResponse.body.case !== "openSession") {
      throw new Error("Native memory daemon omitted OpenSession");
    }
    const session = sessionResponse.body.value;
    this.startHeartbeat(session);
    const acquire = create(AcquireProjectRequestSchema, {
      daemonInstanceId: daemon.daemonInstanceId,
      sessionId: session.sessionId,
      projectRoot: this.worktree,
      worktree: this.worktree,
      ...optionalEnv("OPENCODE_MEMORY_DATA_DIR", "dataDir"),
      ...optionalEnv("OPENCODE_MEMORY_MODEL_CACHE", "modelCache"),
      ...optionalEnv("OPENCODE_MEMORY_INITIAL_PROFILE", "initialProfileId"),
      ...optionalEnv("OPENCODE_MEMORY_EXPECTED_PROFILE", "expectedProfileId"),
      ...contentScanningPolicyFromEnv(),
      embedding: embeddingIdentity(),
    });
    const projectResponse = await this.send(
      { case: "acquireProject", value: acquire },
      INITIALIZATION_TIMEOUT_MS,
    );
    if (projectResponse.body.case !== "acquireProject") {
      throw new Error("Native memory daemon omitted AcquireProject");
    }
    return { daemon, session, project: projectResponse.body.value, generation };
  }

  private async connectTransport(bootstrapIfMissing = true): Promise<void> {
    if (this.disposed) throw new Error("Native memory client is disposed");
    if (this.socket && !this.socket.destroyed) return;
    let socket: Socket;
    try {
      socket = await connectSocket(this.endpoint, CONNECT_TIMEOUT_MS);
    } catch (error) {
      if (!bootstrapIfMissing) throw error;
      await bootstrapDaemon(this.root, this.endpoint);
      socket = await connectSocket(this.endpoint, CONNECT_TIMEOUT_MS);
    }
    if (this.disposed) {
      socket.destroy();
      throw new Error("Native memory client is disposed");
    }
    this.attachSocket(socket);
  }

  private attachSocket(socket: Socket): void {
    this.socket?.destroy();
    this.socket = socket;
    this.generation += 1;
    const generation = this.generation;
    const decoder = new DelimitedFrameDecoder(MAX_RESPONSE_BYTES);
    socket.on("data", (chunk: Buffer) => {
      try {
        for (const frame of decoder.push(chunk)) {
          this.handleFrame(frame, generation);
        }
      } catch (error) {
        socket.destroy(asError(error));
      }
    });
    socket.once("error", (error) => {
      this.rejectPending(
        new DaemonTransportError(`Native memory daemon socket failed: ${error.message}`),
        generation,
      );
    });
    socket.once("close", () => {
      if (this.socket === socket && this.generation === generation) {
        this.socket = undefined;
        this.ready = undefined;
        this.stopHeartbeat();
      }
      this.rejectPending(
        new DaemonTransportError("Native memory daemon connection closed"),
        generation,
      );
    });
  }

  private send(
    body: DaemonRequestMessage["body"],
    timeoutMs: number,
    signal?: AbortSignal,
  ): Promise<DaemonResponse> {
    const socket = this.socket;
    const generation = this.generation;
    if (!socket || socket.destroyed) {
      throw new DaemonTransportError("Native memory daemon is not connected");
    }
    if (signal?.aborted) throw new Error("Native memory request was cancelled");
    const requestId = randomUUID();
    const request = createDaemonRequest(body, requestId);
    const payload = encodeDelimited(toBinary(DaemonRequestSchema, request));
    if (payload.byteLength > MAX_REQUEST_BYTES) {
      throw new Error(
        `Native memory daemon request exceeds ${MAX_REQUEST_BYTES} bytes (was ${payload.byteLength})`,
      );
    }

    return new Promise<DaemonResponse>((resolveRequest, rejectRequest) => {
      let abort: (() => void) | undefined;
      const finish = (): void => {
        const pending = this.pending.get(requestId);
        if (!pending) return;
        this.pending.delete(requestId);
        clearTimeout(pending.timer);
        if (signal && abort) signal.removeEventListener("abort", abort);
      };
      const timer = setTimeout(() => {
        if (!this.pending.has(requestId)) return;
        finish();
        rejectRequest(
          new DaemonRpcError(
            "Native memory daemon request timed out",
            DaemonStatusCode.DEADLINE_EXCEEDED,
          ),
        );
      }, timeoutMs);
      timer.unref?.();
      this.pending.set(requestId, {
        resolve: (response) => {
          finish();
          resolveRequest(response);
        },
        reject: (error) => {
          finish();
          rejectRequest(error);
        },
        timer,
        generation,
      });
      if (signal) {
        abort = () => {
          finish();
          rejectRequest(
            new DaemonRpcError("Native memory request was cancelled", DaemonStatusCode.CANCELLED),
          );
        };
        signal.addEventListener("abort", abort, { once: true });
      }
      socket.write(payload, (error) => {
        if (!error) return;
        const pending = this.pending.get(requestId);
        pending?.reject(new DaemonTransportError(`Cannot write daemon request: ${error.message}`));
      });
    });
  }

  private handleFrame(frame: Uint8Array, generation: number): void {
    const response = fromBinary(DaemonResponseSchema, frame);
    const pending = this.pending.get(response.requestId);
    if (!pending || pending.generation !== generation) return;
    const status = response.status;
    if (!status || status.code !== DaemonStatusCode.OK) {
      pending.reject(
        new DaemonRpcError(
          status?.message || "Native memory daemon request failed",
          status?.code ?? DaemonStatusCode.INTERNAL,
          status?.retryAfterMs ?? 0,
        ),
      );
      return;
    }
    if (response.body.case === undefined) {
      pending.reject(new Error("Native memory daemon response body is missing"));
      return;
    }
    pending.resolve(response);
  }

  private unwrapMemoryResponse<T>(response: MemoryResponse): T {
    const decoded = decodeMemoryResponse(response);
    if (!decoded.ok) {
      throw new NativeMemoryOperationError(decoded.error || "Native memory operation failed");
    }
    return decoded.result as T;
  }

  private unwrapModelResponse<T>(response: ModelResponse, method: ModelMethod): T {
    const decoded = decodeModelResponse(response, method);
    if (!decoded.ok) {
      throw new NativeMemoryOperationError(decoded.error || "Native memory model operation failed");
    }
    return decoded.result as T;
  }

  private unwrapGraphResponse<T>(response: GraphResponse, method: GraphMethod): T {
    const decoded = decodeGraphResponse(response, method);
    if (!decoded.ok) {
      throw new NativeMemoryOperationError(decoded.error || "Native memory graph operation failed");
    }
    return decoded.result as T;
  }

  private async cancelProjectCall(ready: ReadyProject, callId: string): Promise<CancelOutcome> {
    const cancel = create(CancelCallRequestSchema, {
      daemonInstanceId: ready.daemon.daemonInstanceId,
      sessionId: ready.session.sessionId,
      projectHandle: ready.project.projectHandle,
      leaseId: ready.project.leaseId,
      callId,
    });
    const response = await this.send({ case: "cancelCall", value: cancel }, CONNECT_TIMEOUT_MS);
    if (response.body.case !== "cancelCall") {
      throw new Error("Native memory daemon omitted CancelCall");
    }
    return response.body.value.outcome;
  }

  private startHeartbeat(session: OpenSessionResponse): void {
    this.stopHeartbeat();
    const intervalMs = Math.max(1_000, session.heartbeatIntervalSeconds * 1_000);
    const generation = this.generation;
    this.heartbeat = setInterval(() => {
      if (this.heartbeatInFlight || !this.socket || this.socket.destroyed) return;
      this.heartbeatInFlight = true;
      const heartbeat = create(SessionHeartbeatRequestSchema, {
        daemonInstanceId: session.daemonInstanceId,
        sessionId: session.sessionId,
      });
      void this.send({ case: "heartbeat", value: heartbeat }, CONNECT_TIMEOUT_MS)
        .catch(() => this.invalidateConnection(generation))
        .finally(() => {
          if (generation === this.generation) this.heartbeatInFlight = false;
        });
    }, intervalMs);
    this.heartbeat.unref?.();
  }

  private stopHeartbeat(): void {
    if (this.heartbeat) clearInterval(this.heartbeat);
    this.heartbeat = undefined;
    this.heartbeatInFlight = false;
  }

  private invalidateConnection(expectedGeneration = this.generation): void {
    if (expectedGeneration !== this.generation) return;
    this.ready = undefined;
    this.stopHeartbeat();
    const socket = this.socket;
    this.socket = undefined;
    socket?.destroy();
  }

  private rejectPending(error: Error, generation?: number): void {
    for (const pending of [...this.pending.values()]) {
      if (generation !== undefined && pending.generation !== generation) continue;
      pending.reject(error);
    }
  }
}

function createDaemonRequest(body: DaemonRequestMessage["body"], requestId = randomUUID()) {
  return create(DaemonRequestSchema, {
    requestId,
    protocolGeneration: DAEMON_PROTOCOL_GENERATION,
    body,
  });
}

export class NativeMemoryClient {
  private readonly daemon: DaemonProjectClient;

  constructor(root: string, worktree: string, requestTimeoutMs = REQUEST_TIMEOUT_MS) {
    validateRequestTimeout(requestTimeoutMs);
    this.daemon = new DaemonProjectClient(root, worktree, requestTimeoutMs);
  }

  request<T>(method: MemoryMethod, params: unknown = {}, signal?: AbortSignal): Promise<T> {
    return this.daemon.request<T>(method, params, signal);
  }

  dispose(): Promise<void> {
    return this.daemon.dispose();
  }

  async daemonInfo(): Promise<DaemonClientInfo> {
    return await this.daemon.info();
  }
}

type NativeMemoryClientFactory = (root: string, worktree: string) => NativeMemoryClient;

interface NativeMemoryClientPoolEntry {
  readonly client: NativeMemoryClient;
  leases: number;
  closing?: Promise<void>;
}

export interface NativeMemoryClientLease {
  readonly client: NativeMemoryRequester;
  release(): Promise<void>;
}

export class NativeMemoryClientPool {
  private readonly entries = new Map<string, NativeMemoryClientPoolEntry>();

  constructor(
    private readonly createClient: NativeMemoryClientFactory = (root, worktree) =>
      new NativeMemoryClient(root, worktree),
  ) {}

  async acquire(root: string, worktree: string): Promise<NativeMemoryClientLease> {
    const key = daemonPoolKey(worktree);
    for (;;) {
      const current = this.entries.get(key);
      if (current?.closing) {
        await current.closing;
        continue;
      }
      if (current) {
        current.leases += 1;
        return this.createLease(key, current);
      }
      const entry: NativeMemoryClientPoolEntry = {
        client: this.createClient(root, worktree),
        leases: 1,
      };
      this.entries.set(key, entry);
      return this.createLease(key, entry);
    }
  }

  private createLease(key: string, entry: NativeMemoryClientPoolEntry): NativeMemoryClientLease {
    let released = false;
    return {
      client: entry.client,
      release: async () => {
        if (released) return;
        released = true;
        entry.leases -= 1;
        if (entry.leases > 0) return;
        const closing = entry.client.dispose().finally(() => {
          if (this.entries.get(key) === entry) this.entries.delete(key);
        });
        entry.closing = closing;
        await closing;
      },
    };
  }
}

const SHARED_DAEMON_POOL = Symbol.for("@nguyenthdat/opencode-memory/daemon-pool/v1");
const daemonPoolGlobal = globalThis as typeof globalThis & {
  [key: symbol]: NativeMemoryClientPool | undefined;
};
const sharedNativeMemoryClientPool =
  daemonPoolGlobal[SHARED_DAEMON_POOL] ?? new NativeMemoryClientPool();
daemonPoolGlobal[SHARED_DAEMON_POOL] = sharedNativeMemoryClientPool;

export function acquireNativeMemoryClient(
  root: string,
  worktree: string,
): Promise<NativeMemoryClientLease> {
  return sharedNativeMemoryClientPool.acquire(root, worktree);
}

export async function probeNativeMemoryDaemon(root: string): Promise<DaemonControlInfo> {
  const client = new DaemonProjectClient(root, process.cwd(), CONNECT_TIMEOUT_MS);
  return await client.probe();
}

export async function requestNativeMemoryDaemonDrain(
  root: string,
): Promise<DaemonDrainResult | undefined> {
  const client = new DaemonProjectClient(root, process.cwd(), CONNECT_TIMEOUT_MS);
  return await client.requestDrainIfRunning();
}

export function resolveDaemonEndpoint(): string {
  const uid = process.getuid?.() ?? 0;
  const runtimeDirectory =
    process.platform === "linux" && process.env.XDG_RUNTIME_DIR
      ? join(process.env.XDG_RUNTIME_DIR, "opencode-memory")
      : join(tmpdir(), "opencode-memory");
  const endpoint = join(runtimeDirectory, "daemon.sock");
  return Buffer.byteLength(endpoint) <= 100
    ? endpoint
    : join("/tmp", `opencode-memory-${uid}`, "daemon.sock");
}

export function resolveNativeMemoryBinary(root: string): string {
  const platform = `${process.platform}-${process.arch}`;
  const packageName = NATIVE_PACKAGES[platform];
  if (!packageName) {
    throw new Error(
      `Native memory supports only macOS arm64 and glibc Linux arm64/x64, not ${platform}`,
    );
  }
  const override = process.env.OPENCODE_NATIVE_MEMORY_BIN;
  const binaryName = "opencode-memory";
  const packaged = resolvePackagedBinary(packageName, binaryName);
  const candidates = override
    ? [resolve(override)]
    : [
        resolve(root, "target", "release", binaryName),
        resolve(root, "target", "debug", binaryName),
        ...(packaged ? [packaged] : []),
      ];
  for (const candidate of candidates) {
    if (!existsSync(candidate)) continue;
    const binary = realpathSync(candidate);
    if (!override) {
      const library = resolve(
        binary,
        "..",
        "memory-libs",
        process.platform === "darwin" ? "libzvec_c_api.dylib" : "libzvec_c_api.so",
      );
      if (!existsSync(library)) continue;
    }
    return binary;
  }
  throw new Error(
    `Native memory binary was not found. Reinstall with optional dependencies or run \`bun run build:native:release\`. Checked: ${candidates.join(", ")}`,
  );
}

export function assertDaemonVersionCompatible(
  pluginVersion: string,
  daemonVersion: string,
  daemonPid?: number,
): void {
  if (pluginVersion === "development" || pluginVersion === daemonVersion) return;
  const pid = daemonPid === undefined ? "" : ` (pid ${daemonPid})`;
  throw new Error(
    `Native memory daemon version mismatch${pid}: plugin ${pluginVersion}, daemon ${daemonVersion}. ` +
      "Close all OpenCode processes using memory and restart OpenCode to replace the daemon.",
  );
}

export function assertDaemonSchemaCompatible(
  pluginVersion: string,
  daemon: Pick<GetDaemonInfoResponse, "daemonVersion" | "domainSchemaGeneration" | "pid">,
  endpoint: string,
): void {
  if (daemon.domainSchemaGeneration === DOMAIN_SCHEMA_GENERATION) return;
  throw new Error(
    `Native memory daemon domain schema mismatch at ${endpoint}: ` +
      `client ${DOMAIN_SCHEMA_GENERATION}, daemon ${daemon.domainSchemaGeneration} ` +
      `(plugin ${pluginVersion}, daemon ${daemon.daemonVersion}, pid ${daemon.pid}). ` +
      "Close all OpenCode processes using memory and restart the native memory daemon.",
  );
}

function resolvePackagedBinary(packageName: string, binaryName: string): string | undefined {
  try {
    return require.resolve(`${packageName}/bin/${binaryName}`);
  } catch {
    return undefined;
  }
}

function resolvePluginVersion(root: string): string {
  try {
    const packageJson = JSON.parse(readFileSync(resolve(root, "package.json"), "utf8")) as {
      version?: unknown;
    };
    if (typeof packageJson.version === "string" && packageJson.version.length > 0) {
      return packageJson.version;
    }
  } catch {
    // Development callers may not have a package manifest at their root.
  }
  return "development";
}

async function bootstrapDaemon(root: string, endpoint: string): Promise<void> {
  const runtimeDirectory = dirname(endpoint);
  await ensurePrivateRuntimeDirectory(runtimeDirectory);
  const startLock = join(runtimeDirectory, "daemon-start.lock");
  const deadline = Date.now() + START_LOCK_TIMEOUT_MS;
  let lock: FileHandle | undefined;

  while (!lock && Date.now() < deadline) {
    try {
      lock = await open(startLock, "wx", 0o600);
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code;
      if (code !== "EEXIST") throw error;
      if (await canConnect(endpoint)) return;
      await removeStaleStartLock(startLock);
      await delay(50);
    }
  }
  if (!lock)
    throw new Error(`Timed out waiting for native memory daemon startup lock ${startLock}`);

  const lockIdentity = await lock.stat();
  try {
    if (await canConnect(endpoint)) return;
    await lock.writeFile(`${process.pid}\n${Date.now()}\n`);
    const binary = resolveNativeMemoryBinary(root);
    const child = spawn(binary, ["--daemon", "--endpoint", endpoint], {
      cwd: dirname(binary),
      detached: true,
      env: process.env,
      stdio: "ignore",
    });
    let startupFailure: Error | undefined;
    child.once("error", (error) => {
      startupFailure = new Error(`Native memory daemon failed to start: ${error.message}`, {
        cause: error,
      });
    });
    child.once("exit", (code, signal) => {
      if (code !== 0) {
        startupFailure = new Error(
          `Native memory daemon exited during startup with ${code ?? signal ?? "unknown status"}`,
        );
      }
    });
    child.unref();
    const readinessDeadline = Date.now() + STARTUP_TIMEOUT_MS;
    while (Date.now() < readinessDeadline) {
      if (await canConnect(endpoint)) return;
      if (startupFailure) throw startupFailure;
      await delay(50);
    }
    throw new Error(
      `Native memory daemon did not become ready at ${endpoint}. ` +
        "Run the packaged binary with --daemon manually to inspect its stderr.",
    );
  } finally {
    await lock.close();
    const current = await stat(startLock).catch(() => undefined);
    if (current?.dev === lockIdentity.dev && current.ino === lockIdentity.ino) {
      await unlink(startLock).catch(() => undefined);
    }
  }
}

async function removeStaleStartLock(path: string): Promise<void> {
  try {
    const metadata = await stat(path);
    const uid = process.getuid?.();
    if (uid !== undefined && metadata.uid !== uid) {
      throw new Error(`Native memory daemon startup lock has a foreign owner: ${path}`);
    }
    if (Date.now() - metadata.mtimeMs < START_LOCK_STALE_MS) return;
    const ownerPid = Number.parseInt(await readFile(path, "utf8"), 10);
    if (Number.isSafeInteger(ownerPid) && ownerPid > 0 && processIsAlive(ownerPid)) return;
    const current = await stat(path);
    if (current.dev === metadata.dev && current.ino === metadata.ino) await unlink(path);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }
}

async function connectSocket(path: string, timeoutMs: number): Promise<Socket> {
  await validateDaemonEndpoint(path);
  return await new Promise((resolveSocket, rejectSocket) => {
    const socket = createConnection({ path });
    const timer = setTimeout(() => {
      socket.destroy();
      rejectSocket(
        new DaemonTransportError(`Timed out connecting to native memory daemon at ${path}`),
      );
    }, timeoutMs);
    timer.unref?.();
    const onError = (error: Error): void => {
      clearTimeout(timer);
      rejectSocket(
        new DaemonTransportError(`Cannot connect to native memory daemon: ${error.message}`, {
          cause: error,
        }),
      );
    };
    socket.once("error", onError);
    socket.once("connect", () => {
      clearTimeout(timer);
      socket.removeListener("error", onError);
      resolveSocket(socket);
    });
  });
}

async function canConnect(path: string): Promise<boolean> {
  try {
    const socket = await connectSocket(path, 250);
    socket.destroy();
    return true;
  } catch (error) {
    if (isUnavailableEndpointError(error)) return false;
    throw error;
  }
}

function isUnavailableEndpointError(error: unknown): boolean {
  const code = (error as NodeJS.ErrnoException).code;
  if (code === "ENOENT" || code === "ECONNREFUSED") return true;
  if (!(error instanceof DaemonTransportError) || !error.cause) return false;
  const causeCode = (error.cause as NodeJS.ErrnoException).code;
  return causeCode === "ENOENT" || causeCode === "ECONNREFUSED";
}

function daemonDrainOutcome(outcome: DrainOutcome): DaemonDrainOutcome {
  switch (outcome) {
    case DrainOutcome.ACCEPTED:
      return "accepted";
    case DrainOutcome.BUSY:
      return "busy";
    case DrainOutcome.UNSUPPORTED:
      return "unsupported";
    case DrainOutcome.UNSPECIFIED:
      throw new Error("Native memory daemon returned an unspecified drain outcome");
    default:
      throw new Error(`Native memory daemon returned unknown drain outcome ${outcome}`);
  }
}

async function ensurePrivateRuntimeDirectory(path: string): Promise<void> {
  try {
    await mkdir(path, { mode: 0o700 });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
  }
  const metadata = await lstat(path);
  const uid = process.getuid?.();
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    throw new Error(`Native memory runtime path is not a real directory: ${path}`);
  }
  if (uid !== undefined && metadata.uid !== uid) {
    throw new Error(`Native memory runtime directory has a foreign owner: ${path}`);
  }
  await chmod(path, 0o700);
  const restricted = await lstat(path);
  if ((restricted.mode & 0o777) !== 0o700) {
    throw new Error(`Native memory runtime directory must use mode 0700: ${path}`);
  }
}

async function validateDaemonEndpoint(path: string): Promise<void> {
  const runtimeDirectory = dirname(path);
  const directory = await lstat(runtimeDirectory);
  const endpoint = await lstat(path);
  const uid = process.getuid?.();
  if (!directory.isDirectory() || directory.isSymbolicLink()) {
    throw new Error(`Native memory runtime path is not a real directory: ${runtimeDirectory}`);
  }
  if (uid !== undefined && (directory.uid !== uid || endpoint.uid !== uid)) {
    throw new Error(`Native memory daemon endpoint has a foreign owner: ${path}`);
  }
  if ((directory.mode & 0o777) !== 0o700) {
    throw new Error(`Native memory runtime directory must use mode 0700: ${runtimeDirectory}`);
  }
  if (!endpoint.isSocket() || endpoint.isSymbolicLink()) {
    throw new Error(`Native memory daemon endpoint is not a real Unix socket: ${path}`);
  }
  if ((endpoint.mode & 0o777) !== 0o600) {
    throw new Error(`Native memory daemon endpoint must use mode 0600: ${path}`);
  }
}

function processIsAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "EPERM";
  }
}

function embeddingIdentity() {
  return create(EmbeddingIdentitySchema, {
    ...optionalEnv("OPENCODE_MEMORY_EMBEDDING_MODEL_PATH", "localModelPath"),
    ...optionalEnv("OPENCODE_MEMORY_EMBEDDING_MODEL_REPO", "repository"),
    ...optionalEnv("OPENCODE_MEMORY_EMBEDDING_MODEL_REVISION", "revision"),
    ...optionalEnv("OPENCODE_MEMORY_EMBEDDING_MODEL_FILE", "filename"),
    ...optionalEnv("OPENCODE_MEMORY_EMBEDDING_POOLING", "pooling"),
    ...optionalEnv("OPENCODE_MEMORY_EMBEDDING_ATTENTION", "attention"),
    ...optionalEnv("OPENCODE_MEMORY_EMBEDDING_QUERY_TEMPLATE", "queryTemplate"),
    ...optionalEnv("OPENCODE_MEMORY_EMBEDDING_PASSAGE_TEMPLATE", "passageTemplate"),
    ...optionalBooleanEnv("OPENCODE_MEMORY_EMBEDDING_ADD_BOS", "addBos"),
    ...optionalBooleanEnv("OPENCODE_MEMORY_EMBEDDING_APPEND_EOS", "appendEos"),
    ...optionalBooleanEnv("OPENCODE_MEMORY_EMBEDDING_NORMALIZE", "normalize"),
    ...optionalIntegerEnv("OPENCODE_MEMORY_EMBEDDING_DIMENSION", "dimension"),
    ...optionalIntegerEnv("OPENCODE_MEMORY_EMBEDDING_CONTEXT_SIZE", "contextSize"),
    ...optionalPositiveInt32Env("OPENCODE_MEMORY_EMBEDDING_THREADS", "threads"),
    ...optionalIntegerEnv("OPENCODE_MEMORY_EMBEDDING_GPU_LAYERS", "gpuLayers"),
  });
}

function optionalEnv<const K extends string>(name: string, key: K): Partial<Record<K, string>> {
  const value = process.env[name];
  return value ? ({ [key]: value } as Partial<Record<K, string>>) : {};
}

function optionalBooleanEnv<const K extends string>(
  name: string,
  key: K,
): Partial<Record<K, boolean>> {
  const value = process.env[name];
  if (!value) return {};
  if (["1", "true", "yes", "on"].includes(value.toLowerCase())) {
    return { [key]: true } as Partial<Record<K, boolean>>;
  }
  if (["0", "false", "no", "off"].includes(value.toLowerCase())) {
    return { [key]: false } as Partial<Record<K, boolean>>;
  }
  throw new Error(`Invalid ${name}: expected true or false, received ${value}`);
}

function optionalEnabledUnlessDisabledEnv<const K extends string>(
  name: string,
  key: K,
  environment: NodeJS.ProcessEnv = process.env,
): Partial<Record<K, boolean>> {
  const value = environment[name];
  if (!value) return {};
  if (["1", "true", "yes", "on"].includes(value.toLowerCase())) {
    return { [key]: false } as Partial<Record<K, boolean>>;
  }
  if (["0", "false", "no", "off"].includes(value.toLowerCase())) {
    return { [key]: true } as Partial<Record<K, boolean>>;
  }
  throw new Error(`Invalid ${name}: expected true or false, received ${value}`);
}

export function contentScanningPolicyFromEnv(environment: NodeJS.ProcessEnv = process.env): {
  secretScanning?: boolean;
  promptInjectionScanning?: boolean;
} {
  return {
    ...optionalEnabledUnlessDisabledEnv(
      "OPENCODE_MEMORY_DISABLE_SECRET_SCANNER",
      "secretScanning",
      environment,
    ),
    ...optionalEnabledUnlessDisabledEnv(
      "OPENCODE_MEMORY_DISABLE_PROMPT_INJECTION_SCAN",
      "promptInjectionScanning",
      environment,
    ),
  };
}

function optionalIntegerEnv<const K extends string>(
  name: string,
  key: K,
): Partial<Record<K, number>> {
  const value = process.env[name];
  if (!value) return {};
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 0 || parsed > 0xffff_ffff) {
    throw new Error(`Invalid ${name}: expected a uint32-compatible integer, received ${value}`);
  }
  return { [key]: parsed } as Partial<Record<K, number>>;
}

function optionalPositiveInt32Env<const K extends string>(
  name: string,
  key: K,
): Partial<Record<K, number>> {
  const value = process.env[name];
  if (!value) return {};
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0 || parsed > 0x7fff_ffff) {
    throw new Error(`Invalid ${name}: expected a positive int32, received ${value}`);
  }
  return { [key]: parsed } as Partial<Record<K, number>>;
}

function daemonPoolKey(worktree: string): string {
  return [
    resolve(worktree),
    process.env.OPENCODE_MEMORY_DATA_DIR ?? "",
    process.env.OPENCODE_MEMORY_DISABLE_SECRET_SCANNER ?? "",
    process.env.OPENCODE_MEMORY_DISABLE_PROMPT_INJECTION_SCAN ?? "",
  ].join("\0");
}

function configuredRequestTimeoutMs(): number {
  const configured = Number(process.env.OPENCODE_MEMORY_REQUEST_TIMEOUT_MS);
  if (!Number.isFinite(configured) || configured <= 0) return DEFAULT_REQUEST_TIMEOUT_MS;
  return Math.min(Math.max(Math.trunc(configured), MIN_REQUEST_TIMEOUT_MS), MAX_REQUEST_TIMEOUT_MS);
}

function validateRequestTimeout(value: number): void {
  if (!Number.isSafeInteger(value) || value < 1 || value > MAX_REQUEST_TIMEOUT_MS) {
    throw new Error(
      `Invalid native memory request timeout: expected 1-${MAX_REQUEST_TIMEOUT_MS} ms, received ${value}`,
    );
  }
}

function isAmbiguousFailure(error: Error): boolean {
  return (
    error instanceof DaemonTransportError ||
    (error instanceof DaemonRpcError &&
      [
        DaemonStatusCode.CANCELLED,
        DaemonStatusCode.DEADLINE_EXCEEDED,
        DaemonStatusCode.INTERNAL,
        DaemonStatusCode.OUTCOME_UNKNOWN,
      ].includes(error.code))
  );
}

function isDefinitePreAdmissionFailure(error: Error): boolean {
  return (
    error instanceof DaemonRpcError &&
    [
      DaemonStatusCode.INVALID_ARGUMENT,
      DaemonStatusCode.FAILED_PRECONDITION,
      DaemonStatusCode.NOT_FOUND,
      DaemonStatusCode.RESOURCE_EXHAUSTED,
      DaemonStatusCode.UNAVAILABLE,
    ].includes(error.code)
  );
}

function isReconnectRequiredFailure(error: Error): boolean {
  return (
    error instanceof DaemonRpcError &&
    (error.code === DaemonStatusCode.NOT_FOUND ||
      (error.code === DaemonStatusCode.FAILED_PRECONDITION &&
        error.message.includes("daemon instance changed")))
  );
}

function isRetryableSetupFailure(error: Error): boolean {
  return (
    error instanceof DaemonTransportError ||
    (error instanceof DaemonRpcError &&
      [
        DaemonStatusCode.DEADLINE_EXCEEDED,
        DaemonStatusCode.NOT_FOUND,
        DaemonStatusCode.UNAVAILABLE,
      ].includes(error.code))
  );
}

function waitForPromise<T>(promise: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (!signal) return promise;
  if (signal.aborted) {
    throw new DaemonRpcError("Native memory request was cancelled", DaemonStatusCode.CANCELLED);
  }
  return new Promise<T>((resolvePromise, rejectPromise) => {
    const abort = (): void => {
      rejectPromise(
        new DaemonRpcError("Native memory request was cancelled", DaemonStatusCode.CANCELLED),
      );
    };
    signal.addEventListener("abort", abort, { once: true });
    void promise.then(resolvePromise, rejectPromise).finally(() => {
      signal.removeEventListener("abort", abort);
    });
  });
}

function asError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolveDelay) => setTimeout(resolveDelay, milliseconds));
}
