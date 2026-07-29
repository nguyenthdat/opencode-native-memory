import { describe, expect, test } from "bun:test";
import {
  CANDIDATES_CLOSE,
  CANDIDATES_OPEN,
  COMPACTION_CONTEXT,
  deriveRecallQuery,
  extractDirectUserEvidence,
  hasDirectUserEvidence,
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

  test("forbids guessed code paths in the compaction prompt", () => {
    expect(COMPACTION_CONTEXT).not.toContain('"code_paths":["relative/path"]');
    expect(COMPACTION_CONTEXT).toContain("verified existing regular files");
    expect(COMPACTION_CONTEXT).toContain("never infer or guess a path");
    expect(COMPACTION_CONTEXT).toContain(
      "omit a project fact candidate when no verified file applies",
    );
  });

  test("accepts a personal observation only with exact direct-user evidence", () => {
    const personal = {
      title: "default_user preferred language",
      content: "default_user prefers responses in Vietnamese.",
      kind: "preference" as const,
      taxonomy: "user_preference" as const,
      importance: 0.6,
      tags: [],
      code_paths: [],
      evidence_quote: "trả lời bằng tiếng Việt",
    };

    expect(parseCuratedCandidates(candidateBlock([personal]))).toEqual([]);
    expect(
      parseCuratedCandidates(candidateBlock([personal]), [
        "Từ giờ hãy trả lời bằng tiếng Việt cho dự án này.",
      ]),
    ).toEqual([
      {
        title: personal.title,
        content: personal.content,
        kind: personal.kind,
        taxonomy: personal.taxonomy,
        importance: personal.importance,
        tags: [],
        code_paths: [],
      },
    ]);
  });

  test("rejects inferred personal facts and incompatible kinds", () => {
    const base = {
      title: "default_user location",
      content: "default_user lives in Ho Chi Minh City.",
      kind: "fact",
      taxonomy: "user_identity",
      importance: 0.6,
      tags: [],
      code_paths: [],
      evidence_quote: "Tôi sống ở Thành phố Hồ Chí Minh",
    };
    const evidence = ["Tôi sống ở Thành phố Hồ Chí Minh"];

    expect(
      parseCuratedCandidates(candidateBlock([{ ...base, evidence_quote: "Vietnam" }]), evidence),
    ).toEqual([]);
    expect(
      parseCuratedCandidates(candidateBlock([{ ...base, kind: "preference" }]), evidence),
    ).toEqual([]);
  });
});

describe("extractDirectUserEvidence", () => {
  test("keeps only non-synthetic user text", () => {
    expect(
      extractDirectUserEvidence([
        {
          info: { role: "user" },
          parts: [
            { type: "text", text: "Remember this preference" },
            { type: "text", text: "synthetic", synthetic: true },
          ],
        },
        { info: { role: "assistant" }, parts: [{ type: "text", text: "not evidence" }] },
      ]),
    ).toEqual(["Remember this preference"]);
  });

  test("normalizes whitespace but still requires a verbatim substring", () => {
    expect(
      hasDirectUserEvidence("trả lời bằng tiếng Việt", [
        "Hãy   trả lời bằng tiếng Việt\ncho dự án này",
      ]),
    ).toBeTrue();
    expect(hasDirectUserEvidence("I prefer concise answers", ["Please be concise"])).toBeFalse();
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
