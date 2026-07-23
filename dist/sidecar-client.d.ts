import type { ChildProcessWithoutNullStreams } from "node:child_process";
import type { MemoryMethod } from "./protocol.js";
export declare const REQUEST_TIMEOUT_MS = 300000;
export declare const INITIALIZATION_TIMEOUT_MS: number;
export declare const MAX_REQUEST_BYTES: number;
export declare const MAX_RESPONSE_BYTES: number;
export type SpawnFn = (binary: string, args: string[], opts: {
    cwd: string;
    detached: boolean;
    env: NodeJS.ProcessEnv | undefined;
    stdio: ["pipe", "pipe", "pipe"];
}) => ChildProcessWithoutNullStreams;
export declare function resolveNativeMemoryBinary(root: string): string;
export declare class NativeMemoryClient {
    private readonly root;
    private readonly worktree;
    private process;
    private disposed;
    private nextID;
    private pending;
    private generation;
    private handshake;
    private readonly spawnFn;
    private readonly usingSpawnOverride;
    private readonly requestTimeoutMs;
    constructor(root: string, worktree: string, spawnOverride?: SpawnFn, requestTimeoutMs?: number);
    request<T>(method: MemoryMethod, params?: unknown, signal?: AbortSignal): Promise<T>;
    dispose(): Promise<void>;
    private sendRequest;
    private waitForHandshake;
    private ensureHandshake;
    private start;
    private handleFrame;
    private finishPending;
    private rejectGeneration;
    private failProcess;
    private isCurrentAndRunning;
}
//# sourceMappingURL=sidecar-client.d.ts.map