import type { ChildProcessWithoutNullStreams } from "node:child_process";
import { spawn as nodeSpawn } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import { resolve } from "node:path";
import type { PendingRequest, RpcResponse } from "./contracts.js";

const MiB = 1024 * 1024;

export const REQUEST_TIMEOUT_MS = 300_000;
export const MAX_REQUEST_BYTES = 32 * MiB;
export const MAX_RESPONSE_BYTES = 32 * MiB;
const MAX_STDERR_BYTES = 8_192;
const MAX_HANDSHAKE_RESTARTS = 1;

const SUPPORTED_RPC_VERSION = 1;

export type SpawnFn = (
  binary: string,
  args: string[],
  opts: {
    cwd: string;
    detached: boolean;
    env: NodeJS.ProcessEnv | undefined;
    stdio: ["pipe", "pipe", "pipe"];
  },
) => ChildProcessWithoutNullStreams;

export function resolveNativeMemoryBinary(root: string): string {
  const override = process.env.OPENCODE_NATIVE_MEMORY_BIN;
  const candidates = override
    ? [resolve(override)]
    : [
        resolve(root, "target", "release", "opencode-native-memory"),
        resolve(root, "target", "debug", "opencode-native-memory"),
      ];
  for (const candidate of candidates) {
    if (!existsSync(candidate)) continue;
    const binary = realpathSync(candidate);
    if (!override && process.platform !== "win32") {
      const library = resolve(
        binary,
        "..",
        "native-memory-libs",
        process.platform === "darwin"
          ? "libzvec_c_api.dylib"
          : "libzvec_c_api.so",
      );
      if (!existsSync(library)) continue;
    }
    return binary;
  }
  throw new Error(
    `Native memory binary was not found. Run \`bun run memory:build:release\`. Checked: ${candidates.join(", ")}`,
  );
}

interface ProcessState {
  readonly child: ChildProcessWithoutNullStreams;
  readonly generation: number;
}

interface HandshakePromise {
  generation: number;
  promise: Promise<void>;
}

class ProcessReplacedError extends Error {}

export class NativeMemoryClient {
  private process: ProcessState | undefined;
  private disposed = false;
  private nextID = 1;
  private pending = new Map<number, PendingRequest & { generation: number }>();
  private generation = 0;
  private handshake: HandshakePromise | undefined;
  private readonly spawnFn: SpawnFn;
  private readonly usingSpawnOverride: boolean;
  private readonly requestTimeoutMs: number;

  constructor(
    private readonly root: string,
    private readonly worktree: string,
    spawnOverride?: SpawnFn,
    requestTimeoutMs?: number,
  ) {
    this.spawnFn = spawnOverride ?? nodeSpawn;
    this.usingSpawnOverride = spawnOverride !== undefined;
    this.requestTimeoutMs = requestTimeoutMs ?? REQUEST_TIMEOUT_MS;
  }

  // ---- Public API ---------------------------------------------------------

  async request<T>(
    method: string,
    params: unknown = {},
    signal?: AbortSignal,
  ): Promise<T> {
    if (this.disposed) throw new Error("Native memory client is disposed");
    if (signal?.aborted) throw new Error("Native memory request was cancelled");

    if (method === "shutdown") {
      return await this.sendRequest<T>(this.start(), method, params, signal);
    }

    for (let restart = 0; restart <= MAX_HANDSHAKE_RESTARTS; restart += 1) {
      const process = this.start();
      try {
        await this.waitForHandshake(process, signal);
      } catch (error) {
        if (error instanceof ProcessReplacedError) {
          if (restart < MAX_HANDSHAKE_RESTARTS) continue;
          throw new Error(
            "Native memory sidecar exited repeatedly during protocol handshake",
          );
        }
        throw error;
      }
      if (signal?.aborted) throw new Error("Native memory request was cancelled");
      if (!this.isCurrentAndRunning(process)) {
        if (restart < MAX_HANDSHAKE_RESTARTS) continue;
        throw new Error(
          "Native memory sidecar exited repeatedly during protocol handshake",
        );
      }
      return await this.sendRequest<T>(process, method, params, signal);
    }

    throw new Error("Native memory handshake retry limit was exhausted");
  }

  async dispose(): Promise<void> {
    this.disposed = true;
    const ps = this.process;
    if (!ps) return;

    // If process already exited, just clean up
    if (ps.child.exitCode !== null || ps.child.signalCode !== null) {
      this.process = undefined;
      this.rejectGeneration(ps.generation, new Error("Native memory client stopped"));
      return;
    }

    // Register close before sending shutdown to avoid missing the event
    const closePromise = new Promise<void>((resolveClose) => {
      ps.child.once("close", () => resolveClose());
    });

    const forceKill = setTimeout(() => {
      stopProcessTree(ps.child, "SIGKILL");
    }, 1_000);
    forceKill.unref?.();

    // Send shutdown through internal path, bypassing disposed check
    try {
      await this.sendRequest<unknown>(ps, "shutdown", {});
    } catch {
      // Process teardown below is authoritative.
    }
    ps.child.stdin.end();

    try {
      await closePromise;
    } finally {
      clearTimeout(forceKill);
      if (this.process === ps) {
        this.process = undefined;
      }
      this.rejectGeneration(ps.generation, new Error("Native memory client stopped"));
    }
  }

  // ---- Internal: sending requests to a captured process --------------------

  private sendRequest<T>(
    process: ProcessState,
    method: string,
    params: unknown = {},
    signal?: AbortSignal,
  ): Promise<T> {
    if (!this.isCurrentAndRunning(process)) {
      throw new ProcessReplacedError(
        "Native memory process changed before the request was sent",
      );
    }
    if (signal?.aborted) throw new Error("Native memory request was cancelled");

    const id = this.nextID++;
    const payload = `${JSON.stringify({ id, method, params })}\n`;

    const payloadBytes = Buffer.byteLength(payload, "utf8");
    if (payloadBytes > MAX_REQUEST_BYTES) {
      throw new Error(
        `Native memory request payload exceeds ${MAX_REQUEST_BYTES} bytes (was ${payloadBytes})`,
      );
    }

    return new Promise<T>((resolveRequest, rejectRequest) => {
      const timeout = this.requestTimeoutMs;
      const timer = setTimeout(() => {
        const active = this.pending.get(id);
        if (!active) return;
        this.finishPending(id, active);
        rejectRequest(
          new Error(`Native memory ${method} timed out after ${timeout} ms`),
        );
      }, timeout);
      timer.unref?.();

      const entry: PendingRequest & { generation: number } = {
        resolve: (value) => resolveRequest(value as T),
        reject: rejectRequest,
        timer,
        signal,
        generation: process.generation,
      };
      if (signal) {
        entry.abort = () => {
          if (!this.pending.delete(id)) return;
          clearTimeout(timer);
          rejectRequest(new Error(`Native memory ${method} was cancelled`));
        };
        signal.addEventListener("abort", entry.abort, { once: true });
      }
      this.pending.set(id, entry);

      process.child.stdin.write(payload, (error) => {
        if (!error) return;
        const active = this.pending.get(id);
        if (!active) return;
        this.finishPending(id, active);
        active.reject(
          new Error(`Cannot write native memory request: ${error.message}`),
        );
      });
    });
  }

  // ---- Internal: handshake ------------------------------------------------

  private async waitForHandshake(
    process: ProcessState,
    signal?: AbortSignal,
  ): Promise<void> {
    if (signal?.aborted) {
      throw new Error("Native memory request was cancelled");
    }

    const handshake = this.ensureHandshake(process);

    if (!signal) {
      await handshake.promise;
      return;
    }

    let onAbort: (() => void) | undefined;

    const abortPromise = new Promise<never>((_, reject) => {
      if (signal.aborted) {
        reject(new Error("Native memory request was cancelled"));
        return;
      }
      onAbort = () => {
        reject(new Error("Native memory request was cancelled"));
      };
      signal.addEventListener("abort", onAbort, { once: true });
    });

    try {
      await Promise.race([handshake.promise, abortPromise]);
    } finally {
      if (onAbort) {
        signal.removeEventListener("abort", onAbort);
      }
    }
  }

  private ensureHandshake(process: ProcessState): HandshakePromise {
    if (this.handshake?.generation === process.generation) {
      return this.handshake;
    }

    const handshake: HandshakePromise = {
      generation: process.generation,
      promise: Promise.resolve(),
    };
    this.handshake = handshake;
    handshake.promise = Promise.resolve().then(async () => {
      const status = await this.sendRequest<{
        rpc_protocol_version: number;
      }>(process, "status", {});
      const protocolVer = status.rpc_protocol_version;
      if (protocolVer !== SUPPORTED_RPC_VERSION) {
        if (protocolVer === undefined || protocolVer === null) {
          throw new Error(
            `Native memory backend does not report its RPC protocol version. ` +
              `Run \`bun run memory:build:release\` to rebuild the binary.`,
          );
        }
        throw new Error(
          `Native memory RPC protocol version mismatch: ` +
            `client supports ${SUPPORTED_RPC_VERSION}, backend reports ${protocolVer}. ` +
            `Run \`bun run memory:build:release\` to rebuild the binary.`,
        );
      }
    });
    return handshake;
  }

  // ---- Internal: process management ---------------------------------------

  private start(): ProcessState {
    if (this.process && this.isCurrentAndRunning(this.process)) return this.process;

    const binary = this.usingSpawnOverride
      ? (process.env.OPENCODE_NATIVE_MEMORY_BIN ??
        resolve(this.root, "target", "release", "opencode-native-memory"))
      : resolveNativeMemoryBinary(this.root);
    const child = this.spawnFn(binary, [], {
      cwd: this.worktree,
      detached: process.platform !== "win32",
      env: {
        ...process.env,
        OPENCODE_MEMORY_PROJECT_ROOT: this.worktree,
      },
      stdio: ["pipe", "pipe", "pipe"],
    });

    this.generation += 1;
    const gen = this.generation;
    this.process = { child, generation: gen };
    this.handshake = undefined;

    let stdoutChunks: Buffer[] = [];
    let stdoutBytes = 0;
    let stderr = "";
    const processState = this.process;

    child.stdout.on("data", (chunk: Buffer) => {
      let offset = 0;
      while (offset < chunk.length) {
        const newline = chunk.indexOf(0x0a, offset);
        const end = newline < 0 ? chunk.length : newline;
        const segment = chunk.subarray(offset, end);
        if (stdoutBytes + segment.length > MAX_RESPONSE_BYTES) {
          this.failProcess(
            processState,
            new Error(`Native memory response exceeds ${MAX_RESPONSE_BYTES} bytes`),
          );
          return;
        }
        if (segment.length > 0) {
          stdoutChunks.push(segment);
          stdoutBytes += segment.length;
        }
        if (newline < 0) return;
        const frame =
          stdoutChunks.length === 1
            ? stdoutChunks[0]!
            : Buffer.concat(stdoutChunks, stdoutBytes);
        this.handleLine(frame.toString("utf8"), gen, processState);
        stdoutChunks = [];
        stdoutBytes = 0;
        offset = newline + 1;
      }
    });

    child.stderr.on("data", (chunk: Buffer) => {
      stderr = `${stderr}${chunk.toString("utf8")}`.slice(-MAX_STDERR_BYTES);
    });

    child.stdin.on("error", (error: Error) => {
      this.failProcess(
        processState,
        new Error(`Native memory stdin failed: ${error.message}`),
      );
    });

    child.once("error", (error: NodeJS.ErrnoException) => {
      const hint =
        error.code === "ENOENT"
          ? "Run `bun run memory:build:release`."
          : error.message;
      this.failProcess(
        processState,
        new Error(`Native memory failed to start: ${hint}`),
      );
    });

    child.once("exit", (code: number | null, signal: string | null) => {
      if (this.process?.generation === gen) this.process = undefined;
      if (this.handshake?.generation === gen) this.handshake = undefined;
      this.rejectGeneration(
        gen,
        new ProcessReplacedError(
          `Native memory exited with ${code ?? signal ?? "unknown status"}`,
        ),
      );
    });

    child.once("close", (code: number | null, signal: string | null) => {
      if (this.process?.generation !== gen) return;
      this.process = undefined;
      if (this.disposed && code === 0) return;
      const detail = stderr.trim();
      this.rejectGeneration(
        gen,
        new Error(
          `Native memory exited with ${code ?? signal ?? "unknown status"}${detail ? `: ${detail}` : ""}`,
        ),
      );
    });

    return processState;
  }

  // ---- Internal: line handling (generation-aware) -------------------------

  private handleLine(line: string, gen: number, process: ProcessState): void {
    let response: RpcResponse;
    try {
      response = JSON.parse(line) as RpcResponse;
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      this.failProcess(
        process,
        new Error(`Native memory returned invalid JSON: ${detail}`),
      );
      return;
    }

    // An id=0 error applies to the whole protocol generation.
    if (response.id === 0) {
      this.failProcess(
        process,
        new Error(response.error || "Native memory protocol error"),
      );
      return;
    }

    if (!Number.isSafeInteger(response.id) || typeof response.ok !== "boolean") {
      this.failProcess(
        process,
        new Error("Native memory returned an invalid response"),
      );
      return;
    }

    const pending = this.pending.get(response.id);
    // Only resolve/reject if the pending belongs to this generation
    if (!pending || pending.generation !== gen) return;
    this.finishPending(response.id, pending);
    if (response.ok) pending.resolve(response.result);
    else
      pending.reject(
        new Error(response.error || "Native memory operation failed"),
      );
  }

  private finishPending(
    id: number,
    pending: PendingRequest & { generation: number },
  ): void {
    this.pending.delete(id);
    clearTimeout(pending.timer);
    if (pending.signal && pending.abort) {
      pending.signal.removeEventListener("abort", pending.abort);
    }
  }

  private rejectGeneration(gen: number, error: Error): void {
    for (const [id, pending] of this.pending) {
      if (pending.generation !== gen) continue;
      this.finishPending(id, pending);
      pending.reject(error);
    }
  }

  private failProcess(process: ProcessState, error: Error): void {
    if (this.process?.generation === process.generation) this.process = undefined;
    if (this.handshake?.generation === process.generation) this.handshake = undefined;
    this.rejectGeneration(process.generation, error);
    stopProcessTree(process.child, "SIGTERM");
  }

  private isCurrentAndRunning(process: ProcessState): boolean {
    return (
      this.process?.generation === process.generation &&
      process.child.exitCode === null &&
      process.child.signalCode === null
    );
  }
}

function stopProcessTree(
  child: ChildProcessWithoutNullStreams,
  signal: NodeJS.Signals,
): void {
  if (!child.pid) return;
  if (child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform !== "win32") {
    try {
      process.kill(-child.pid, signal);
      return;
    } catch {
      // Fall back to the direct child.
    }
  }
  try {
    child.kill(signal);
  } catch {
    // Process already exited.
  }
}
