import { describe, expect, test } from "bun:test";
import {
  assertDaemonVersionCompatible,
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
    expect(() => assertDaemonVersionCompatible("0.6.0-beta.2", "0.6.0-beta.0", 64346)).toThrow(
      "Close all OpenCode processes using memory and restart OpenCode",
    );
    expect(() => assertDaemonVersionCompatible("0.6.0-beta.2", "0.6.0-beta.2")).not.toThrow();
    expect(() => assertDaemonVersionCompatible("development", "0.6.0-beta.0")).not.toThrow();
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
