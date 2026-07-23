import { describe, expect, test } from "bun:test";
import type { MemoryRecord } from "./contracts.js";
import { validateDeleteRecords } from "./validation.js";

describe("validateDeleteRecords", () => {
  test("allows local memory scopes", () => {
    expect(() =>
      validateDeleteRecords([
        record("mem_project", "project", "session:test"),
        record("mem_agent", "agent", "session:test"),
      ]),
    ).not.toThrow();
  });

  test("rejects repository records with their canonical source", () => {
    expect(() =>
      validateDeleteRecords([
        record("mem_repository", "repository", ".opencode/memory/decisions/canonical.md"),
      ]),
    ).toThrow(
      "mem_repository (.opencode/memory/decisions/canonical.md). Edit or remove their .opencode/memory files instead.",
    );
  });
});

function record(
  id: string,
  scope: MemoryRecord["scope"],
  source: string,
): Pick<MemoryRecord, "id" | "scope" | "source"> {
  return { id, scope, source };
}
