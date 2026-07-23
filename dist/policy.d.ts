import type { CuratedCandidate, SearchResponse } from "./contracts.js";
export declare const CANDIDATES_OPEN = "<durable-memory-candidates>";
export declare const CANDIDATES_CLOSE = "</durable-memory-candidates>";
export declare const COMPACTION_CONTEXT = "Preserve durable project knowledge across compaction, but never copy the full summary into memory. Exclude secrets, guesses, transient progress, and conversational detail. End the summary with exactly this block containing a JSON array of at most three verified, atomic candidates (or []):\n<durable-memory-candidates>\n[{\"title\":\"...\",\"content\":\"...\",\"kind\":\"decision|preference|fact|pattern|gotcha\",\"importance\":0.0,\"tags\":[\"...\"],\"code_paths\":[\"relative/path\"]}]\n</durable-memory-candidates>\nImportance must be between 0 and 0.6 inclusive. Facts require at least one code_paths entry. Do not include Markdown fences.";
interface RecallQueryPart {
    type: string;
    text?: unknown;
    synthetic?: unknown;
    ignored?: unknown;
    filename?: unknown;
    mime?: unknown;
    url?: unknown;
    source?: unknown;
}
export declare function deriveRecallQuery(parts: readonly RecallQueryPart[]): string | undefined;
export declare function formatRecalledMemories(response: SearchResponse, budgetChars: number): {
    text: string;
    memoryIDs: string[];
} | undefined;
export declare function truncateText(value: string, maxCharacters: number): string;
export declare function contextBudgetChars(model: {
    limit?: {
        context?: number;
    };
}): number;
export declare function safeJson(value: unknown): string;
export declare function parseCuratedCandidates(content: string): CuratedCandidate[];
export {};
//# sourceMappingURL=policy.d.ts.map