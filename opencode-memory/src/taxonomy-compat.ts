import { MEMORY_KINDS, MEMORY_TAXONOMIES } from "./contracts.js";

export const MEMORY_TAXONOMY_INPUTS = [...MEMORY_TAXONOMIES, "gotcha"] as const;

export type MemoryKindInput = (typeof MEMORY_KINDS)[number];
export type MemoryTaxonomy = (typeof MEMORY_TAXONOMIES)[number];
export type MemoryTaxonomyInput = (typeof MEMORY_TAXONOMY_INPUTS)[number];

function requireGotchaKind(kind: MemoryKindInput): void {
  if (kind !== "gotcha") {
    throw new Error("taxonomy 'gotcha' is a compatibility alias and requires kind 'gotcha'");
  }
}

export function normalizeCreateTaxonomy(
  kind: MemoryKindInput,
  taxonomy: MemoryTaxonomyInput | undefined,
): MemoryTaxonomy | undefined {
  if (taxonomy !== "gotcha") return taxonomy;
  requireGotchaKind(kind);
  return undefined;
}

export function normalizeUpdateTaxonomy(
  effectiveKind: MemoryKindInput,
  taxonomy: MemoryTaxonomyInput | undefined,
): MemoryTaxonomy | undefined {
  if (taxonomy !== "gotcha") return taxonomy;
  requireGotchaKind(effectiveKind);
  return "fix_pattern";
}

export function normalizeTaxonomyFilters(
  kinds: readonly MemoryKindInput[],
  taxonomies: readonly MemoryTaxonomyInput[],
): { kinds: MemoryKindInput[]; taxonomies: MemoryTaxonomy[] } {
  if (!taxonomies.includes("gotcha")) {
    return {
      kinds: [...kinds],
      taxonomies: [...taxonomies] as MemoryTaxonomy[],
    };
  }
  if (taxonomies.length !== 1) {
    throw new Error(
      "taxonomy filter 'gotcha' selects the gotcha memory kind and cannot be combined with taxonomy filters",
    );
  }
  return {
    kinds: [...new Set([...kinds, "gotcha" as const])],
    taxonomies: [],
  };
}
