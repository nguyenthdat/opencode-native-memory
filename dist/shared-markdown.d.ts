import type { MemoryRecord, SharedMemoryRecord } from "./contracts.js";
export declare const SHARED_MEMORY_RELATIVE_DIR = ".opencode/memory";
export declare function loadSharedMemories(worktree: string): Promise<{
    records: SharedMemoryRecord[];
    signature: string;
}>;
export declare function parseSharedMemory(source: string, input: string): SharedMemoryRecord;
export declare function writeSharedMemory(worktree: string, memory: MemoryRecord): Promise<string>;
export declare function ensureRealDirectory(path: string): Promise<void>;
export declare function isPathWithin(root: string, candidate: string): boolean;
//# sourceMappingURL=shared-markdown.d.ts.map