import { describe, expect, test } from "bun:test";
import {
  CANDIDATES_CLOSE,
  CANDIDATES_OPEN,
  COMPACTION_CONTEXT,
  deriveRecallQuery,
  parseCuratedCandidates,
} from "./policy.js";

describe("deriveRecallQuery", () => {
  test("uses only eligible user text when text is present", () => {
    expect(
      deriveRecallQuery([
        { type: "text", text: "  inspect cache behavior  " },
        { type: "text", text: "synthetic", synthetic: true },
        { type: "text", text: "ignored", ignored: true },
        { type: "file", filename: "fallback.ts" },
      ]),
    ).toBe("inspect cache behavior");
  });

  test("derives textless queries from file and symbol metadata", () => {
    expect(
      deriveRecallQuery([
        {
          type: "file",
          filename: "ignored-fallback.ts",
          source: { type: "symbol", name: "SessionContext", path: "src/session-context.ts" },
        },
        { type: "file", source: { type: "file", path: "src/policy.ts" } },
        { type: "file", filename: "notes.md" },
      ]),
    ).toBe("Symbol: SessionContext (src/session-context.ts)\nFile: src/policy.ts\nFile: notes.md");
  });

  test("does not derive a query from attachment URLs or MIME types", () => {
    expect(
      deriveRecallQuery([
        { type: "file", mime: "image/png", url: "data:image/png;base64,secret" },
        { type: "text", text: "   " },
      ]),
    ).toBeUndefined();
  });
});

describe("parseCuratedCandidates", () => {
  test("keeps valid siblings when another candidate is malformed", () => {
    const candidates = parseCuratedCandidates(
      candidateBlock([
        validCandidate("First"),
        { ...validCandidate("Invalid"), importance: 0.8 },
        validCandidate("Third"),
      ]),
    );

    expect(candidates.map((candidate) => candidate.title)).toEqual(["First", "Third"]);
  });

  test("accepts at most three independently valid candidates", () => {
    const candidates = parseCuratedCandidates(
      candidateBlock([
        { broken: true },
        validCandidate("One"),
        validCandidate("Two"),
        validCandidate("Three"),
        validCandidate("Four"),
      ]),
    );

    expect(candidates.map((candidate) => candidate.title)).toEqual(["One", "Two", "Three"]);
  });

  test("states the automatic importance ceiling in the compaction prompt", () => {
    expect(COMPACTION_CONTEXT).toContain("Importance must be between 0 and 0.6 inclusive");
  });
});

function candidateBlock(candidates: unknown[]): string {
  return `${CANDIDATES_OPEN}\n${JSON.stringify(candidates)}\n${CANDIDATES_CLOSE}`;
}

function validCandidate(title: string): Record<string, unknown> {
  return {
    title,
    content: `${title} content`,
    kind: "decision",
    importance: 0.6,
    tags: [],
    code_paths: [],
  };
}
