import { describe, expect, test } from "bun:test";

import {
  normalizeCreateTaxonomy,
  normalizeTaxonomyFilters,
  normalizeUpdateTaxonomy,
} from "../src/taxonomy-compat.js";

describe("taxonomy compatibility", () => {
  test("normalizes gotcha writes through kind inference", () => {
    expect(normalizeCreateTaxonomy("gotcha", "gotcha")).toBeUndefined();
    expect(normalizeUpdateTaxonomy("gotcha", "gotcha")).toBe("fix_pattern");
  });

  test("rejects gotcha taxonomy alias for another kind", () => {
    expect(() => normalizeCreateTaxonomy("fact", "gotcha")).toThrow("requires kind 'gotcha'");
    expect(() => normalizeUpdateTaxonomy("pattern", "gotcha")).toThrow("requires kind 'gotcha'");
  });

  test("converts gotcha taxonomy filters into kind filters", () => {
    expect(normalizeTaxonomyFilters([], ["gotcha"])).toEqual({
      kinds: ["gotcha"],
      taxonomies: [],
    });
    expect(normalizeTaxonomyFilters(["fact"], ["gotcha"])).toEqual({
      kinds: ["fact", "gotcha"],
      taxonomies: [],
    });
  });

  test("rejects ambiguous gotcha and taxonomy filter unions", () => {
    expect(() => normalizeTaxonomyFilters([], ["gotcha", "fix_pattern"])).toThrow(
      "cannot be combined",
    );
  });

  test("preserves regular taxonomies and filters", () => {
    expect(normalizeCreateTaxonomy("fact", "architecture_fact")).toBe("architecture_fact");
    expect(normalizeTaxonomyFilters(["fact"], ["architecture_fact"])).toEqual({
      kinds: ["fact"],
      taxonomies: ["architecture_fact"],
    });
  });
});
