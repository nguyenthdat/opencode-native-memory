import type { Plugin } from "@opencode-ai/plugin";
export interface MemoryPluginOptions {
    root: string;
    warmup?: boolean;
}
export declare function createMemoryPlugin(options: MemoryPluginOptions): Plugin;
//# sourceMappingURL=plugin.d.ts.map