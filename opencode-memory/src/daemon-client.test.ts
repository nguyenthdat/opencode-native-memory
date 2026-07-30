import { describe, expect, test } from "bun:test";
import {
  assertDaemonVersionCompatible,
  assertDaemonSchemaCompatible,
  isRetrySafeMemoryMethod,
  NativeMemoryClient,
  NativeMemoryClientPool,
  resolveDaemonEndpoint,
} from "./daemon-client.js";

class TrackingClient extends NativeMemoryClient {
  disposeCalls = 0;

  constructor() {
    super(".", ".");
  }

  override async dispose(): Promise<void> {
    this.disposeCalls += 1;
  }
}

describe("shared daemon client", () => {
  test("never maps an ordinary client request to global daemon shutdown", async () => {
    const client = new NativeMemoryClient(".", ".");
    await expect(client.request("shutdown")).rejects.toThrow("cannot shut down the user daemon");
    await client.dispose();
  });

  test("uses a short absolute user-scoped Unix socket endpoint", () => {
    const endpoint = resolveDaemonEndpoint();
    expect(endpoint.startsWith("/")).toBe(true);
    expect(Buffer.byteLength(endpoint)).toBeLessThanOrEqual(100);
    expect(endpoint.endsWith("/daemon.sock")).toBe(true);
  });

  test("rejects invalid custom request timeouts before opening a transport", () => {
    expect(() => new NativeMemoryClient(".", ".", 0)).toThrow("request timeout");
    expect(() => new NativeMemoryClient(".", ".", Number.NaN)).toThrow("request timeout");
  });

  test("rejects a stale daemon version but permits development clients", () => {
    expect(() => assertDaemonVersionCompatible("0.6.0", "0.6.0-beta.2", 64346)).toThrow(
      "Close all OpenCode processes using memory and restart OpenCode",
    );
    expect(() => assertDaemonVersionCompatible("0.6.0", "0.6.0")).not.toThrow();
    expect(() => assertDaemonVersionCompatible("development", "0.6.0-beta.0")).not.toThrow();
  });

  test("reports both domain schema generations for a stale daemon", () => {
    expect(() =>
      assertDaemonSchemaCompatible(
        "0.6.0",
        { daemonVersion: "0.6.0", domainSchemaGeneration: 2, pid: 64346 },
        "/tmp/opencode-memory/daemon.sock",
      ),
    ).toThrow(
      "Native memory daemon domain schema mismatch at /tmp/opencode-memory/daemon.sock: client 4, daemon 2 (plugin 0.6.0, daemon 0.6.0, pid 64346)",
    );
    expect(() =>
      assertDaemonSchemaCompatible(
        "0.6.0",
        { daemonVersion: "0.6.0", domainSchemaGeneration: 4, pid: 64346 },
        "/tmp/opencode-memory/daemon.sock",
      ),
    ).not.toThrow();
  });

  test("preserves model retry classifications", () => {
    expect(isRetrySafeMemoryMethod("model_profiles")).toBe(true);
    expect(isRetrySafeMemoryMethod("model_switch_status")).toBe(true);
    expect(isRetrySafeMemoryMethod("model_switch")).toBe(false);
    expect(isRetrySafeMemoryMethod("model_switch_cancel")).toBe(false);
    expect(isRetrySafeMemoryMethod("graph_extract_prepare")).toBe(true);
    expect(isRetrySafeMemoryMethod("graph_extract_enqueue")).toBe(true);
    expect(isRetrySafeMemoryMethod("graph_extract_claim")).toBe(true);
    expect(isRetrySafeMemoryMethod("graph_extract_renew")).toBe(true);
    expect(isRetrySafeMemoryMethod("graph_extract_job_status")).toBe(true);
    expect(isRetrySafeMemoryMethod("graph_extract_cancel")).toBe(true);
    expect(isRetrySafeMemoryMethod("graph_extract_complete")).toBe(false);
    expect(isRetrySafeMemoryMethod("graph_extract_fail")).toBe(false);
    expect(isRetrySafeMemoryMethod("graph_run_status")).toBe(true);
    expect(isRetrySafeMemoryMethod("graph_search")).toBe(true);
    expect(isRetrySafeMemoryMethod("graph_status")).toBe(true);
    expect(isRetrySafeMemoryMethod("graph_export")).toBe(true);
    expect(isRetrySafeMemoryMethod("graph_upsert_candidates")).toBe(false);
  });

  test("releases the shared project client only after the final local lease", async () => {
    const clients: TrackingClient[] = [];
    const pool = new NativeMemoryClientPool(() => {
      const client = new TrackingClient();
      clients.push(client);
      return client;
    });
    const first = await pool.acquire("/plugin-a", "/tmp/shared-daemon-project");
    const second = await pool.acquire("/plugin-b", "/tmp/shared-daemon-project");

    expect(first.client).toBe(second.client);
    await first.release();
    expect(clients[0]?.disposeCalls).toBe(0);
    await second.release();
    expect(clients[0]?.disposeCalls).toBe(1);
  });
});
