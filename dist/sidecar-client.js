import { spawn as nodeSpawn } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { decodeResponse, DelimitedFrameDecoder, encodeRequest } from "./protocol.js";
const MiB = 1024 * 1024;
export const REQUEST_TIMEOUT_MS = 300_000;
export const INITIALIZATION_TIMEOUT_MS = 30 * 60_000;
export const MAX_REQUEST_BYTES = 32 * MiB;
export const MAX_RESPONSE_BYTES = 32 * MiB;
const MAX_STDERR_BYTES = 8_192;
const MAX_HANDSHAKE_RESTARTS = 1;
const SUPPORTED_RPC_VERSION = 2;
const require = createRequire(import.meta.url);
const NATIVE_PACKAGES = {
    "darwin-arm64": "@nguyenthdat/opencode-memory-darwin-arm64",
    "linux-arm64": "@nguyenthdat/opencode-memory-linux-arm64-gnu",
    "linux-x64": "@nguyenthdat/opencode-memory-linux-x64-gnu",
};
export function resolveNativeMemoryBinary(root) {
    const platform = `${process.platform}-${process.arch}`;
    const packageName = NATIVE_PACKAGES[platform];
    if (!packageName) {
        throw new Error(`Native memory supports only macOS arm64 and glibc Linux arm64/x64, not ${platform}`);
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
        if (!existsSync(candidate))
            continue;
        const binary = realpathSync(candidate);
        if (!override) {
            const library = resolve(binary, "..", "memory-libs", process.platform === "darwin" ? "libzvec_c_api.dylib" : "libzvec_c_api.so");
            if (!existsSync(library))
                continue;
        }
        return binary;
    }
    throw new Error(`Native memory binary was not found. Reinstall with optional dependencies or run \`bun run build:native:release\`. Checked: ${candidates.join(", ")}`);
}
function resolvePackagedBinary(packageName, binaryName) {
    try {
        return require.resolve(`${packageName}/bin/${binaryName}`);
    }
    catch {
        return undefined;
    }
}
class ProcessReplacedError extends Error {
}
export class NativeMemoryClient {
    root;
    worktree;
    process;
    disposed = false;
    nextID = 1;
    pending = new Map();
    generation = 0;
    handshake;
    spawnFn;
    usingSpawnOverride;
    requestTimeoutMs;
    constructor(root, worktree, spawnOverride, requestTimeoutMs) {
        this.root = root;
        this.worktree = worktree;
        this.spawnFn = spawnOverride ?? nodeSpawn;
        this.usingSpawnOverride = spawnOverride !== undefined;
        this.requestTimeoutMs = requestTimeoutMs ?? REQUEST_TIMEOUT_MS;
    }
    // ---- Public API ---------------------------------------------------------
    async request(method, params = {}, signal) {
        if (this.disposed)
            throw new Error("Native memory client is disposed");
        if (signal?.aborted)
            throw new Error("Native memory request was cancelled");
        if (method === "shutdown") {
            return await this.sendRequest(this.start(), method, params, signal);
        }
        for (let restart = 0; restart <= MAX_HANDSHAKE_RESTARTS; restart += 1) {
            const process = this.start();
            try {
                await this.waitForHandshake(process, signal);
            }
            catch (error) {
                if (error instanceof ProcessReplacedError) {
                    if (restart < MAX_HANDSHAKE_RESTARTS)
                        continue;
                    throw new Error("Native memory sidecar exited repeatedly during protocol handshake");
                }
                throw error;
            }
            if (signal?.aborted)
                throw new Error("Native memory request was cancelled");
            if (!this.isCurrentAndRunning(process)) {
                if (restart < MAX_HANDSHAKE_RESTARTS)
                    continue;
                throw new Error("Native memory sidecar exited repeatedly during protocol handshake");
            }
            return await this.sendRequest(process, method, params, signal);
        }
        throw new Error("Native memory handshake retry limit was exhausted");
    }
    async dispose() {
        this.disposed = true;
        const ps = this.process;
        if (!ps)
            return;
        // If process already exited, just clean up
        if (ps.child.exitCode !== null || ps.child.signalCode !== null) {
            this.process = undefined;
            this.rejectGeneration(ps.generation, new Error("Native memory client stopped"));
            return;
        }
        // Register close before sending shutdown to avoid missing the event
        const closePromise = new Promise((resolveClose) => {
            ps.child.once("close", () => resolveClose());
        });
        const forceKill = setTimeout(() => {
            stopProcessTree(ps.child, "SIGKILL");
        }, 1_000);
        forceKill.unref?.();
        // Send shutdown through internal path, bypassing disposed check
        try {
            await this.sendRequest(ps, "shutdown", {});
        }
        catch {
            // Process teardown below is authoritative.
        }
        ps.child.stdin.end();
        try {
            await closePromise;
        }
        finally {
            clearTimeout(forceKill);
            if (this.process === ps) {
                this.process = undefined;
            }
            this.rejectGeneration(ps.generation, new Error("Native memory client stopped"));
        }
    }
    // ---- Internal: sending requests to a captured process --------------------
    sendRequest(process, method, params = {}, signal, timeoutMs = this.requestTimeoutMs) {
        if (!this.isCurrentAndRunning(process)) {
            throw new ProcessReplacedError("Native memory process changed before the request was sent");
        }
        if (signal?.aborted)
            throw new Error("Native memory request was cancelled");
        const id = this.nextID++;
        const payload = encodeRequest(id, method, params);
        const payloadBytes = payload.byteLength;
        if (payloadBytes > MAX_REQUEST_BYTES) {
            throw new Error(`Native memory request payload exceeds ${MAX_REQUEST_BYTES} bytes (was ${payloadBytes})`);
        }
        return new Promise((resolveRequest, rejectRequest) => {
            const timeout = timeoutMs;
            const timer = setTimeout(() => {
                const active = this.pending.get(id);
                if (!active)
                    return;
                this.finishPending(id, active);
                rejectRequest(new Error(`Native memory ${method} timed out after ${timeout} ms`));
            }, timeout);
            timer.unref?.();
            const entry = {
                resolve: (value) => resolveRequest(value),
                reject: rejectRequest,
                timer,
                signal,
                generation: process.generation,
            };
            if (signal) {
                entry.abort = () => {
                    if (!this.pending.delete(id))
                        return;
                    clearTimeout(timer);
                    rejectRequest(new Error(`Native memory ${method} was cancelled`));
                };
                signal.addEventListener("abort", entry.abort, { once: true });
            }
            this.pending.set(id, entry);
            process.child.stdin.write(payload, (error) => {
                if (!error)
                    return;
                const active = this.pending.get(id);
                if (!active)
                    return;
                this.finishPending(id, active);
                active.reject(new Error(`Cannot write native memory request: ${error.message}`));
            });
        });
    }
    // ---- Internal: handshake ------------------------------------------------
    async waitForHandshake(process, signal) {
        if (signal?.aborted) {
            throw new Error("Native memory request was cancelled");
        }
        const handshake = this.ensureHandshake(process);
        if (!signal) {
            await handshake.promise;
            return;
        }
        let onAbort;
        const abortPromise = new Promise((_, reject) => {
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
        }
        finally {
            if (onAbort) {
                signal.removeEventListener("abort", onAbort);
            }
        }
    }
    ensureHandshake(process) {
        if (this.handshake?.generation === process.generation) {
            return this.handshake;
        }
        const handshake = {
            generation: process.generation,
            promise: Promise.resolve(),
        };
        this.handshake = handshake;
        handshake.promise = Promise.resolve().then(async () => {
            const status = await this.sendRequest(process, "status", {}, undefined, INITIALIZATION_TIMEOUT_MS);
            const protocolVer = status.rpc_protocol_version;
            if (protocolVer !== SUPPORTED_RPC_VERSION) {
                if (protocolVer === undefined || protocolVer === null) {
                    throw new Error(`Native memory backend does not report its RPC protocol version. ` +
                        `Run \`bun run build:native:release\` to rebuild the binary.`);
                }
                throw new Error(`Native memory RPC protocol version mismatch: ` +
                    `client supports ${SUPPORTED_RPC_VERSION}, backend reports ${protocolVer}. ` +
                    `Run \`bun run build:native:release\` to rebuild the binary.`);
            }
        });
        return handshake;
    }
    // ---- Internal: process management ---------------------------------------
    start() {
        if (this.process && this.isCurrentAndRunning(this.process))
            return this.process;
        const binary = this.usingSpawnOverride
            ? (process.env.OPENCODE_NATIVE_MEMORY_BIN ??
                resolve(this.root, "target", "release", "opencode-memory"))
            : resolveNativeMemoryBinary(this.root);
        const child = this.spawnFn(binary, [], {
            cwd: this.worktree,
            detached: true,
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
        const frameDecoder = new DelimitedFrameDecoder(MAX_RESPONSE_BYTES);
        let stderr = "";
        const processState = this.process;
        child.stdout.on("data", (chunk) => {
            try {
                for (const frame of frameDecoder.push(chunk)) {
                    this.handleFrame(frame, gen, processState);
                }
            }
            catch (error) {
                const detail = error instanceof Error ? error.message : String(error);
                this.failProcess(processState, new Error(detail));
            }
        });
        child.stderr.on("data", (chunk) => {
            stderr = `${stderr}${chunk.toString("utf8")}`.slice(-MAX_STDERR_BYTES);
        });
        child.stdin.on("error", (error) => {
            this.failProcess(processState, new Error(`Native memory stdin failed: ${error.message}`));
        });
        child.once("error", (error) => {
            const hint = error.code === "ENOENT" ? "Run `bun run memory:build:release`." : error.message;
            this.failProcess(processState, new Error(`Native memory failed to start: ${hint}`));
        });
        child.once("exit", (code, signal) => {
            if (this.process?.generation === gen)
                this.process = undefined;
            if (this.handshake?.generation === gen)
                this.handshake = undefined;
            this.rejectGeneration(gen, new ProcessReplacedError(`Native memory exited with ${code ?? signal ?? "unknown status"}`));
        });
        child.once("close", (code, signal) => {
            if (this.process?.generation !== gen)
                return;
            this.process = undefined;
            if (this.disposed && code === 0)
                return;
            const detail = stderr.trim();
            this.rejectGeneration(gen, new Error(`Native memory exited with ${code ?? signal ?? "unknown status"}${detail ? `: ${detail}` : ""}`));
        });
        return processState;
    }
    // ---- Internal: Protobuf frame handling (generation-aware) ---------------
    handleFrame(frame, gen, process) {
        let response;
        try {
            response = decodeResponse(frame);
        }
        catch (error) {
            const detail = error instanceof Error ? error.message : String(error);
            this.failProcess(process, new Error(`Native memory returned invalid Protobuf: ${detail}`));
            return;
        }
        // An id=0 error applies to the whole protocol generation.
        if (response.id === 0) {
            this.failProcess(process, new Error(response.error || "Native memory protocol error"));
            return;
        }
        if (!Number.isSafeInteger(response.id) || typeof response.ok !== "boolean") {
            this.failProcess(process, new Error("Native memory returned an invalid response"));
            return;
        }
        const pending = this.pending.get(response.id);
        // Only resolve/reject if the pending belongs to this generation
        if (!pending || pending.generation !== gen)
            return;
        this.finishPending(response.id, pending);
        if (response.ok)
            pending.resolve(response.result);
        else
            pending.reject(new Error(response.error || "Native memory operation failed"));
    }
    finishPending(id, pending) {
        this.pending.delete(id);
        clearTimeout(pending.timer);
        if (pending.signal && pending.abort) {
            pending.signal.removeEventListener("abort", pending.abort);
        }
    }
    rejectGeneration(gen, error) {
        for (const [id, pending] of this.pending) {
            if (pending.generation !== gen)
                continue;
            this.finishPending(id, pending);
            pending.reject(error);
        }
    }
    failProcess(process, error) {
        if (this.process?.generation === process.generation)
            this.process = undefined;
        if (this.handshake?.generation === process.generation)
            this.handshake = undefined;
        this.rejectGeneration(process.generation, error);
        stopProcessTree(process.child, "SIGTERM");
    }
    isCurrentAndRunning(process) {
        return (this.process?.generation === process.generation &&
            process.child.exitCode === null &&
            process.child.signalCode === null);
    }
}
export class NativeMemoryClientPool {
    createClient;
    entries = new Map();
    constructor(createClient = (root, worktree) => new NativeMemoryClient(root, worktree)) {
        this.createClient = createClient;
    }
    async acquire(root, worktree) {
        const key = sidecarPoolKey(worktree);
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
            const entry = {
                client: this.createClient(root, worktree),
                leases: 1,
            };
            this.entries.set(key, entry);
            return this.createLease(key, entry);
        }
    }
    createLease(key, entry) {
        let released = false;
        return {
            client: entry.client,
            release: async () => {
                if (released)
                    return;
                released = true;
                entry.leases -= 1;
                if (entry.leases > 0)
                    return;
                const closing = entry.client.dispose().finally(() => {
                    if (this.entries.get(key) === entry)
                        this.entries.delete(key);
                });
                entry.closing = closing;
                await closing;
            },
        };
    }
}
const SHARED_SIDECAR_POOL = Symbol.for("@nguyenthdat/opencode-memory/sidecar-pool/v1");
const sidecarPoolGlobal = globalThis;
const sharedNativeMemoryClientPool = sidecarPoolGlobal[SHARED_SIDECAR_POOL] ?? new NativeMemoryClientPool();
sidecarPoolGlobal[SHARED_SIDECAR_POOL] = sharedNativeMemoryClientPool;
export function acquireNativeMemoryClient(root, worktree) {
    return sharedNativeMemoryClientPool.acquire(root, worktree);
}
function sidecarPoolKey(worktree) {
    const absolute = resolve(worktree);
    let canonical = absolute;
    try {
        canonical = realpathSync(absolute);
    }
    catch {
        // Preserve lazy startup when the host provides a path that is not ready yet.
    }
    return `${canonical}\0${process.env.OPENCODE_MEMORY_DATA_DIR ?? ""}`;
}
function stopProcessTree(child, signal) {
    if (!child.pid)
        return;
    if (child.exitCode !== null || child.signalCode !== null)
        return;
    try {
        process.kill(-child.pid, signal);
        return;
    }
    catch {
        // Fall back to the direct child.
    }
    try {
        child.kill(signal);
    }
    catch {
        // Process already exited.
    }
}
//# sourceMappingURL=sidecar-client.js.map