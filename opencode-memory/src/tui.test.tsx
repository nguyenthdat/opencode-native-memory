/** @jsxImportSource @opentui/solid */
import { describe, expect, test } from "bun:test";
import { createRequire } from "node:module";
import { testRender } from "@opentui/solid";
import { createSignal as pluginCreateSignal } from "solid-js";
import type { NativeMemoryStatus } from "./contracts.js";
import type { NativeMemoryClientLease } from "./daemon-client.js";
import type { MemoryMethod } from "./protocol.js";
import memoryTui, { createMemoryTui, memoryHealthText, requestHealthStatus } from "./tui.js";

interface TestCommand {
  name: string;
  slashName?: string;
  run?: () => void;
}

const nativeStatus: NativeMemoryStatus = {
  ready: true,
  rpc_protocol_version: 2,
  backend: "zvec+llama.cpp",
  zvec_version: "0.1.0",
  embedding_model: "test-model",
  embedding_dimension: 2560,
  project_root: "/project",
  project_id: "project-id",
  collection_path: "/data/collection",
  document_count: 1,
  indexed_document_count: 1,
  state_schema_version: 4,
  metadata_count: 1,
  tombstone_count: 0,
  retrieval_count: 0,
  pending_upsert_count: 0,
  pending_delete_count: 0,
  indexes: [{ name: "embedding", completeness: 1 }],
  capabilities: ["test_v1"],
};

const require = createRequire(import.meta.url);
const rendererCreateSignal = (
  require("solid-js/dist/solid.js") as { createSignal: typeof pluginCreateSignal }
).createSignal;
const usesSharedSolidRuntime = pluginCreateSignal === rendererCreateSignal;

describe("memory TUI plugin", () => {
  test("registers a persistent health slot and refresh command", async () => {
    const layers: Array<{ commands?: TestCommand[] }> = [];
    const slots: Array<{ slots?: { app_bottom?: () => unknown } }> = [];
    const disposers: Array<() => void | Promise<void>> = [];
    const toasts: Array<{
      variant?: string;
      title?: string;
      message: string;
      duration?: number;
    }> = [];
    let requests = 0;
    let releases = 0;
    const controller = new AbortController();
    const lease: NativeMemoryClientLease = {
      client: {
        async request<T>(): Promise<T> {
          requests += 1;
          return nativeStatus as T;
        },
      },
      async release() {
        releases += 1;
      },
    };
    const tui = createMemoryTui("/plugin", {
      acquireClient: async () => lease,
      refreshIntervalMs: 60_000,
    });
    const api = {
      state: { path: { worktree: "/project", directory: "/project" } },
      slots: {
        register(slot: { slots?: { app_bottom?: () => unknown } }) {
          slots.push(slot);
          return "memory-health";
        },
      },
      keymap: {
        registerLayer(layer: { commands?: TestCommand[] }) {
          layers.push(layer);
          return () => undefined;
        },
      },
      lifecycle: {
        signal: controller.signal,
        onDispose(disposer: () => void | Promise<void>) {
          disposers.push(disposer);
          return () => undefined;
        },
      },
      ui: {
        toast(toast: { variant?: string; title?: string; message: string; duration?: number }) {
          toasts.push(toast);
        },
      },
    };

    await tui(api as never, undefined, {} as never);
    await Promise.resolve();

    expect(memoryTui.id).toBe("@nguyenthdat/opencode-memory");
    expect(slots[0]?.slots?.app_bottom).toBeFunction();
    expect(layers[0]?.commands).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          name: "memory.health.refresh",
          slashName: "memory-health",
        }),
      ]),
    );
    expect(requests).toBe(1);

    layers[0]?.commands?.[0]?.run?.();
    await Promise.resolve();
    await Promise.resolve();
    expect(toasts).toContainEqual({
      variant: "success",
      title: "Memory health",
      message: "Native memory backend is ready",
      duration: 5_000,
    });

    controller.abort();
    await Promise.all(disposers.map((dispose) => dispose()));
    expect(releases).toBe(1);
  });

  test.skipIf(!usesSharedSolidRuntime)(
    "updates the mounted health badge after the initial check",
    async () => {
      const slots: Array<{ slots?: { app_bottom?: () => unknown } }> = [];
      const disposers: Array<() => void | Promise<void>> = [];
      const controller = new AbortController();
      let resolveStatus: (status: NativeMemoryStatus) => void = () => undefined;
      const pendingStatus = new Promise<NativeMemoryStatus>((resolve) => {
        resolveStatus = resolve;
      });
      const lease: NativeMemoryClientLease = {
        client: {
          async request<T>(): Promise<T> {
            return (await pendingStatus) as T;
          },
        },
        async release() {},
      };
      const tui = createMemoryTui("/plugin", {
        acquireClient: async () => lease,
        refreshIntervalMs: 60_000,
      });
      const api = {
        state: { path: { worktree: "/project", directory: "/project" } },
        slots: {
          register(slot: { slots?: { app_bottom?: () => unknown } }) {
            slots.push(slot);
            return "memory-health";
          },
        },
        keymap: {
          registerLayer() {
            return () => undefined;
          },
        },
        lifecycle: {
          signal: controller.signal,
          onDispose(disposer: () => void | Promise<void>) {
            disposers.push(disposer);
            return () => undefined;
          },
        },
        ui: { toast() {} },
        theme: {
          current: {
            success: "#00ff00",
            warning: "#ffff00",
            error: "#ff0000",
            textMuted: "#888888",
          },
        },
      };

      await tui(api as never, undefined, {} as never);
      const appBottom = slots[0]?.slots?.app_bottom;
      expect(appBottom).toBeFunction();
      const rendered = await testRender(() => appBottom?.() as never, { width: 40, height: 2 });

      try {
        await rendered.flush();
        expect(rendered.captureCharFrame()).toContain("Memory: checking");

        resolveStatus(nativeStatus);
        const frame = await rendered.waitForFrame((next) => next.includes("Memory: healthy"));
        expect(frame).not.toContain("Memory: checking");
      } finally {
        controller.abort();
        await Promise.all(disposers.map((dispose) => dispose()));
        rendered.renderer.destroy();
      }
    },
  );

  test("formats each badge state", () => {
    expect(memoryHealthText({ status: "checking", ready: false, message: "checking" })).toBe(
      "Memory: checking",
    );
    expect(memoryHealthText({ status: "healthy", ready: true, message: "ready" })).toBe(
      "Memory: healthy",
    );
    expect(memoryHealthText({ status: "unavailable", ready: false, message: "down" })).toBe(
      "Memory: unavailable",
    );
  });

  test("times out a health request that never completes", async () => {
    const controller = new AbortController();
    let aborted = false;
    const client: NativeMemoryClientLease["client"] = {
      async request<T>(_method: MemoryMethod, _params?: unknown, signal?: AbortSignal): Promise<T> {
        await new Promise<void>((resolve) => {
          signal?.addEventListener(
            "abort",
            () => {
              aborted = true;
              resolve();
            },
            { once: true },
          );
        });
        return await new Promise<T>(() => undefined);
      },
    };

    await expect(requestHealthStatus(client, controller.signal, 5)).rejects.toThrow(
      "Memory health check timed out after 5 ms",
    );
    expect(aborted).toBe(true);
  });
});
