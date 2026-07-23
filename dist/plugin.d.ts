import type { Plugin } from "@opencode-ai/plugin";
export interface MemoryPluginOptions {
    root: string;
    warmup?: boolean;
    automaticRecall?: boolean;
    automaticCapture?: boolean;
    sharedSync?: boolean;
    feedbackTracking?: boolean;
    minScore?: number;
}
export declare function createMemoryPlugin(options: MemoryPluginOptions): Plugin;
interface ResolvedMemoryPluginOptions {
    warmup: boolean;
    automaticRecall: boolean;
    automaticCapture: boolean;
    sharedSync: boolean;
    feedbackTracking: boolean;
    minScore: number;
}
export declare function resolveMemoryPluginOptions(options: MemoryPluginOptions): ResolvedMemoryPluginOptions;
export {};
//# sourceMappingURL=plugin.d.ts.map