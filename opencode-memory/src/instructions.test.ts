import { afterEach, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import {
  MEMORY_INSTRUCTIONS_MARKER,
  loadMemoryInstructions,
  registerMemoryInstructions,
} from "./instructions.js";

const roots: string[] = [];

afterEach(async () => {
  await Promise.all(roots.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe("memory instructions", () => {
  test("loads the managed package asset", async () => {
    const root = await createRoot();

    const asset = await loadMemoryInstructions(root);

    expect(asset.path).toBe(await realpath(join(root, "rules", "flow.md")));
    expect(asset.content).toContain(MEMORY_INSTRUCTIONS_MARKER);
  });

  test("registers once while preserving existing instructions", async () => {
    const root = await createRoot();
    const asset = await loadMemoryInstructions(root);
    const config = { instructions: ["AGENTS.md"] };

    await registerMemoryInstructions(config, asset, root);
    await registerMemoryInstructions(config, asset, root);

    expect(config.instructions).toEqual(["AGENTS.md", asset.path]);
  });

  test("recognizes a relative symlink to the package asset", async () => {
    const root = await createRoot();
    const asset = await loadMemoryInstructions(root);
    await symlink(asset.path, join(root, "memory-rule.md"));
    const config = { instructions: ["memory-rule.md"] };

    await registerMemoryInstructions(config, asset, root);

    expect(config.instructions).toEqual(["memory-rule.md"]);
  });

  test("rejects an asset without the managed marker", async () => {
    const root = await createRoot("# Wrong rule\n");

    await expect(loadMemoryInstructions(root)).rejects.toThrow("invalid marker");
  });
});

async function createRoot(content = `${MEMORY_INSTRUCTIONS_MARKER}\n# Test\n`): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "opencode-memory-instructions-"));
  roots.push(root);
  const path = join(root, "rules", "flow.md");
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, content);
  return root;
}
