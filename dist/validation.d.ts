import type { MemoryRecord } from "./contracts.js";
export declare function validateUpdateArgs(args: Record<string, unknown>): void;
export declare function validateDeleteRecords(records: readonly Pick<MemoryRecord, "id" | "scope" | "source">[]): void;
//# sourceMappingURL=validation.d.ts.map