import type { Plugin } from "@opencode-ai/plugin";
export interface MemoryPluginOptions {
    root: string;
    warmup?: boolean;
    automaticRecall?: boolean;
    automaticCapture?: boolean;
    automaticDocumentIndex?: boolean;
    documentIndexDebounceMs?: number;
    sharedSync?: boolean;
    feedbackTracking?: boolean;
    minScore?: number;
    projectRoot?: string;
}
export declare function createMemoryPlugin(options: MemoryPluginOptions): Plugin;
interface ResolvedMemoryPluginOptions {
    warmup: boolean;
    automaticRecall: boolean;
    automaticCapture: boolean;
    automaticDocumentIndex: boolean;
    documentIndexDebounceMs: number;
    sharedSync: boolean;
    feedbackTracking: boolean;
    minScore: number;
}
export declare function resolveMemoryPluginOptions(options: MemoryPluginOptions): ResolvedMemoryPluginOptions;
export declare function isSupportedDocumentPath(path: string): boolean;
export {};
//# sourceMappingURL=plugin.d.ts.map