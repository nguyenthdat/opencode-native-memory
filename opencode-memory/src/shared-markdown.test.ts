import { afterEach, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { isPathWithin, loadSharedMemories } from "./shared-markdown.js";

const worktrees: string[] = [];

afterEach(async () => {
  await Promise.all(worktrees.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe("loadSharedMemories", () => {
  test("recurses and keeps valid records when a sibling is malformed", async () => {
    const worktree = await createWorktree();
    await writeShared(worktree, "nested/decision.md", sharedMemory("Nested decision"));
    await writeShared(worktree, "broken.md", "not frontmatter");

    const loaded = await loadSharedMemories(worktree);

    expect(loaded.records.map((record) => record.source)).toEqual([
      ".opencode/memory/nested/decision.md",
    ]);
    expect(loaded.errors).toHaveLength(1);
    expect(loaded.errors[0]?.source).toBe(".opencode/memory/broken.md");
    expect(loaded.errors[0]?.message).toContain("missing YAML frontmatter");
  });

  test("reports an oversized file without suppressing valid records", async () => {
    const worktree = await createWorktree();
    await writeShared(worktree, "valid.md", sharedMemory("Valid"));
    await writeShared(worktree, "oversized.md", "x".repeat(65_537));

    const loaded = await loadSharedMemories(worktree);

    expect(loaded.records.map((record) => record.title)).toEqual(["Valid"]);
    expect(loaded.errors).toHaveLength(1);
    expect(loaded.errors[0]?.message).toContain("exceeds 65536 bytes");
  });

  test("applies the file-count limit across nested directories", async () => {
    const worktree = await createWorktree();
    await Promise.all(
      Array.from({ length: 201 }, (_, index) =>
        writeShared(worktree, `nested/${index}.md`, sharedMemory(`Memory ${index}`)),
      ),
    );

    await expect(loadSharedMemories(worktree)).rejects.toThrow(
      "At most 200 shared memory files are allowed",
    );
  });
});

test("isPathWithin accepts child names that merely start with two dots", () => {
  expect(isPathWithin("/project", "/project/..notes")).toBe(true);
  expect(isPathWithin("/project", "/outside")).toBe(false);
});

async function createWorktree(): Promise<string> {
  const worktree = await mkdtemp(join(tmpdir(), "opencode-memory-test-"));
  worktrees.push(worktree);
  return worktree;
}

async function writeShared(worktree: string, relativePath: string, content: string): Promise<void> {
  const path = join(worktree, ".opencode", "memory", relativePath);
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, content);
}

function sharedMemory(title: string): string {
  return `---
schema_version: 1
title: ${JSON.stringify(title)}
kind: decision
importance: 0.6
tags: []
code_paths: []
---

${title} content
`;
}
