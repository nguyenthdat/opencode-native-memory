import type { NativeMemoryStatus, OptimizeResponse } from "./contracts.js";
import type { NativeMemoryRequester } from "./daemon-client.js";
import { requestIdempotently } from "./outcome-reconciliation.js";

export const DEFAULT_OPTIMIZE_DEBOUNCE_MS = 5_000;
export const DEFAULT_OPTIMIZE_INDEX_THRESHOLD = 1;

export interface MemoryMaintenanceOptions {
  enabled?: boolean;
  debounceMs?: number;
  indexThreshold?: number;
  onError?: (error: unknown) => void;
}

export class MemoryMaintenanceScheduler {
  private timer: ReturnType<typeof setTimeout> | undefined;
  private inFlight: Promise<void> | undefined;
  private pending = false;
  private disposed = false;
  private readonly enabled: boolean;
  private readonly debounceMs: number;
  private readonly indexThreshold: number;
  private readonly onError: (error: unknown) => void;

  constructor(
    private readonly native: NativeMemoryRequester,
    options: MemoryMaintenanceOptions = {},
  ) {
    this.enabled = options.enabled ?? true;
    this.debounceMs = options.debounceMs ?? DEFAULT_OPTIMIZE_DEBOUNCE_MS;
    this.indexThreshold = options.indexThreshold ?? DEFAULT_OPTIMIZE_INDEX_THRESHOLD;
    this.onError = options.onError ?? (() => undefined);
    if (!Number.isFinite(this.debounceMs) || this.debounceMs < 50 || this.debounceMs > 60_000) {
      throw new Error("memory optimizeDebounceMs must be between 50 and 60000");
    }
    if (
      !Number.isFinite(this.indexThreshold) ||
      this.indexThreshold <= 0 ||
      this.indexThreshold > 1
    ) {
      throw new Error("memory optimize indexThreshold must be between 0 and 1");
    }
  }

  schedule(): void {
    if (!this.enabled || this.disposed) return;
    this.pending = true;
    if (this.timer || this.inFlight) return;
    this.timer = setTimeout(() => {
      this.timer = undefined;
      void this.run();
    }, this.debounceMs);
    this.timer.unref?.();
  }

  observeStatus(
    status: Pick<NativeMemoryStatus, "indexes" | "pending_upsert_count" | "pending_delete_count"> &
      Partial<Pick<NativeMemoryStatus, "ready">>,
  ): void {
    if (status.ready === false) return;
    if (
      status.pending_upsert_count > 0 ||
      status.pending_delete_count > 0 ||
      status.indexes.some((index) => index.completeness < this.indexThreshold)
    ) {
      this.schedule();
    }
  }

  async flush(): Promise<void> {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
    while (!this.disposed && (this.pending || this.inFlight)) {
      if (!this.inFlight && this.pending) await this.run();
      else await this.inFlight;
    }
  }

  async dispose(): Promise<void> {
    this.disposed = true;
    this.pending = false;
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
    await this.inFlight?.catch(() => undefined);
  }

  private async run(): Promise<void> {
    if (this.disposed || !this.pending) return;
    this.pending = false;
    const operation = requestIdempotently<OptimizeResponse>(this.native, "optimize", {})
      .then(() => undefined)
      .catch((error) => {
        this.onError(error);
      })
      .finally(() => {
        if (this.inFlight === operation) this.inFlight = undefined;
        if (this.pending && !this.disposed) this.schedule();
      });
    this.inFlight = operation;
    await operation;
  }
}
