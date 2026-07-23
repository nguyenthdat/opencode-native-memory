import { readFile, realpath, stat } from "node:fs/promises";
import { isAbsolute, resolve } from "node:path";

export const MEMORY_INSTRUCTIONS_MARKER = "<!-- opencode-memory-instructions:v1 -->";
const MEMORY_INSTRUCTIONS_RELATIVE_PATH = "rules/flow.md";
const MAX_INSTRUCTIONS_BYTES = 16 * 1024;

export interface MemoryInstructionsAsset {
  path: string;
  content: string;
}

interface InstructionsConfig {
  instructions?: string[];
}

export async function loadMemoryInstructions(
  packageRoot: string,
): Promise<MemoryInstructionsAsset> {
  const candidate = resolve(packageRoot, MEMORY_INSTRUCTIONS_RELATIVE_PATH);
  const info = await stat(candidate).catch((error: unknown) => {
    throw new Error(`Native memory instructions are missing: ${candidate}`, { cause: error });
  });
  if (!info.isFile()) throw new Error(`Native memory instructions are not a file: ${candidate}`);
  if (info.size > MAX_INSTRUCTIONS_BYTES) {
    throw new Error(
      `Native memory instructions exceed ${MAX_INSTRUCTIONS_BYTES} bytes: ${candidate}`,
    );
  }
  const content = await readFile(candidate, "utf8");
  if (!content.includes(MEMORY_INSTRUCTIONS_MARKER)) {
    throw new Error(`Native memory instructions have an invalid marker: ${candidate}`);
  }
  return { path: await realpath(candidate), content };
}

export async function registerMemoryInstructions(
  config: InstructionsConfig,
  asset: MemoryInstructionsAsset,
  projectDirectory: string,
): Promise<void> {
  const instructions = config.instructions ?? [];
  for (const instruction of instructions) {
    const candidate = isAbsolute(instruction)
      ? instruction
      : resolve(projectDirectory, instruction);
    if (resolve(candidate) === asset.path || (await existingRealpath(candidate)) === asset.path)
      return;
  }
  config.instructions = [...instructions, asset.path];
}

async function existingRealpath(path: string): Promise<string | undefined> {
  try {
    return await realpath(path);
  } catch {
    return undefined;
  }
}
