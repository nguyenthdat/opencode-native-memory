export declare const MEMORY_INSTRUCTIONS_MARKER = "<!-- opencode-memory-instructions:v1 -->";
export interface MemoryInstructionsAsset {
    path: string;
    content: string;
}
interface InstructionsConfig {
    instructions?: string[];
}
export declare function loadMemoryInstructions(packageRoot: string): Promise<MemoryInstructionsAsset>;
export declare function registerMemoryInstructions(config: InstructionsConfig, asset: MemoryInstructionsAsset, projectDirectory: string): Promise<void>;
export {};
//# sourceMappingURL=instructions.d.ts.map