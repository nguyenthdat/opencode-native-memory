import { describe, expect, test } from "bun:test";
import { computeRetrievalMetrics } from "./metrics.js";

describe("retrieval benchmark metrics", () => {
  test("scores perfect answerable and abstention results", () => {
    const metrics = computeRetrievalMetrics([
      {
        answerable: true,
        returned: ["a", "b"],
        relevance: [
          { fixture_id: "a", grade: 3 },
          { fixture_id: "b", grade: 1 },
        ],
      },
      { answerable: false, returned: [], relevance: [] },
    ]);

    expect(metrics.hit_at["1"]).toBe(1);
    expect(metrics.mrr_at_10).toBe(1);
    expect(metrics.ndcg_at_10).toBe(1);
    expect(metrics.false_abstention_rate).toBe(0);
    expect(metrics.no_answer_abstention_specificity).toBe(1);
  });

  test("reports misses and false abstention without NaN values", () => {
    const metrics = computeRetrievalMetrics([
      {
        answerable: true,
        returned: [],
        relevance: [{ fixture_id: "a", grade: 3 }],
      },
      { answerable: false, returned: ["noise"], relevance: [] },
    ]);

    expect(metrics.recall_at["10"]).toBe(0);
    expect(metrics.mrr_at_10).toBe(0);
    expect(metrics.false_abstention_rate).toBe(1);
    expect(metrics.no_answer_abstention_specificity).toBe(0);
    expect(Number.isFinite(metrics.ndcg_at_10)).toBe(true);
  });
});
