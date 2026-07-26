import { afterEach, describe, expect, test } from "bun:test";
import { isSupportedDocumentPath, resolveMemoryPluginOptions } from "./plugin.js";

const ENVIRONMENT_KEYS = [
  "OPENCODE_MEMORY_WARMUP",
  "OPENCODE_MEMORY_AUTO_RECALL",
  "OPENCODE_MEMORY_AUTO_CAPTURE",
  "OPENCODE_MEMORY_AUTO_INDEX_DOCUMENTS",
  "OPENCODE_MEMORY_DOCUMENT_INDEX_DEBOUNCE_MS",
  "OPENCODE_MEMORY_SHARED_SYNC",
  "OPENCODE_MEMORY_FEEDBACK_TRACKING",
  "OPENCODE_MEMORY_MIN_SCORE",
] as const;
const original = Object.fromEntries(ENVIRONMENT_KEYS.map((key) => [key, process.env[key]]));

afterEach(() => {
  for (const key of ENVIRONMENT_KEYS) {
    const value = original[key];
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  }
});

describe("memory plugin options", () => {
  test("uses safe defaults", () => {
    for (const key of ENVIRONMENT_KEYS) delete process.env[key];
    expect(resolveMemoryPluginOptions({ root: "/tmp/plugin" })).toEqual({
      warmup: true,
      automaticRecall: true,
      automaticCapture: true,
      automaticDocumentIndex: true,
      documentIndexDebounceMs: 750,
      sharedSync: true,
      feedbackTracking: true,
      minScore: 0.42,
    });
  });

  test("reads environment controls and lets explicit options win", () => {
    process.env.OPENCODE_MEMORY_AUTO_RECALL = "off";
    process.env.OPENCODE_MEMORY_AUTO_CAPTURE = "0";
    process.env.OPENCODE_MEMORY_AUTO_INDEX_DOCUMENTS = "off";
    process.env.OPENCODE_MEMORY_DOCUMENT_INDEX_DEBOUNCE_MS = "1500";
    process.env.OPENCODE_MEMORY_MIN_SCORE = "0.55";
    const resolved = resolveMemoryPluginOptions({
      root: "/tmp/plugin",
      automaticRecall: true,
    });
    expect(resolved.automaticRecall).toBe(true);
    expect(resolved.automaticCapture).toBe(false);
    expect(resolved.automaticDocumentIndex).toBe(false);
    expect(resolved.documentIndexDebounceMs).toBe(1500);
    expect(resolved.minScore).toBe(0.55);
  });

  test("rejects invalid environment values", () => {
    process.env.OPENCODE_MEMORY_SHARED_SYNC = "sometimes";
    expect(() => resolveMemoryPluginOptions({ root: "/tmp/plugin" })).toThrow(
      "OPENCODE_MEMORY_SHARED_SYNC must be a boolean",
    );
  });

  test("recognizes document watcher paths case-insensitively", () => {
    expect(isSupportedDocumentPath("docs/guide.MD")).toBe(true);
    expect(isSupportedDocumentPath("papers/result.pdf")).toBe(true);
    expect(isSupportedDocumentPath("src/main.rs")).toBe(false);
  });

  test("rejects unsafe document index debounce values", () => {
    process.env.OPENCODE_MEMORY_DOCUMENT_INDEX_DEBOUNCE_MS = "10";
    expect(() => resolveMemoryPluginOptions({ root: "/tmp/plugin" })).toThrow(
      "memory documentIndexDebounceMs must be between 50 and 60000",
    );
  });
});
