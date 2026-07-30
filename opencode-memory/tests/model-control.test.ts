import { describe, expect, test } from "bun:test";
import type { NativeMemoryRequester } from "../src/daemon-client.js";
import { listModelProfiles, preflightModelSwitch } from "../src/model-control.js";
import type { MemoryMethod } from "../src/protocol.js";

describe("model control", () => {
  test("profile listing is one side-effect-free native request", async () => {
    const calls: Array<{ method: MemoryMethod; params: unknown }> = [];
    const native: NativeMemoryRequester = {
      async request<T>(method: MemoryMethod, params?: unknown): Promise<T> {
        calls.push({ method, params });
        return { profiles: [] } as T;
      },
    };

    await listModelProfiles(native);
    expect(calls).toEqual([{ method: "model_profiles", params: {} }]);
  });

  test("model switch helper always builds a dry-run preflight", async () => {
    const calls: Array<{ method: MemoryMethod; params: unknown }> = [];
    const native: NativeMemoryRequester = {
      async request<T>(method: MemoryMethod, params?: unknown): Promise<T> {
        calls.push({ method, params });
        return { state: "preflight" } as T;
      },
    };

    await preflightModelSwitch(native, {
      profile_id: "qwen3-text-0.6b-q8",
      allow_dense_downtime: true,
      force_rebuild: true,
      expected_active_profile_id: "qwen3-text-4b-q4",
    });
    expect(calls).toEqual([
      {
        method: "model_switch",
        params: {
          target_profile_id: "qwen3-text-0.6b-q8",
          allow_dense_downtime: true,
          force_rebuild: true,
          expected_active_profile_id: "qwen3-text-4b-q4",
          retain_previous: true,
          dry_run: true,
        },
      },
    ]);
  });
});
