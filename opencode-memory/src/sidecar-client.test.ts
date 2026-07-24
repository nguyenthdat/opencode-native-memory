import { describe, expect, test } from "bun:test";
import {
  NativeMemoryClient,
  NativeMemoryClientPool,
  resolveNativeMemoryBinary,
} from "./sidecar-client.js";

test("rejects Intel macOS as unsupported", () => {
  const platform = process.platform;
  const arch = process.arch;
  Object.defineProperty(process, "platform", { value: "darwin" });
  Object.defineProperty(process, "arch", { value: "x64" });

  try {
    expect(() => resolveNativeMemoryBinary(".")).toThrow(
      "Native memory supports only macOS arm64 and glibc Linux arm64/x64, not darwin-x64",
    );
  } finally {
    Object.defineProperty(process, "platform", { value: platform });
    Object.defineProperty(process, "arch", { value: arch });
  }
});

class TrackingClient extends NativeMemoryClient {
  disposeCalls = 0;

  constructor(private readonly finishImmediately = true) {
    super(".", ".");
  }

  private finishDisposal: (() => void) | undefined;

  override async dispose(): Promise<void> {
    this.disposeCalls += 1;
    if (this.finishImmediately) return;
    await new Promise<void>((resolve) => {
      this.finishDisposal = resolve;
    });
  }

  finish(): void {
    this.finishDisposal?.();
  }
}

describe("native memory client pool", () => {
  test("shares one client until the final same-project lease is released", async () => {
    const clients: TrackingClient[] = [];
    const pool = new NativeMemoryClientPool(() => {
      const client = new TrackingClient();
      clients.push(client);
      return client;
    });

    const first = await pool.acquire("/plugin-a", "/tmp/native-memory-project");
    const second = await pool.acquire("/plugin-b", "/tmp/./native-memory-project");

    expect(first.client).toBe(second.client);
    expect(clients).toHaveLength(1);

    await first.release();
    await first.release();
    expect(clients[0]?.disposeCalls).toBe(0);

    await second.release();
    expect(clients[0]?.disposeCalls).toBe(1);

    const replacement = await pool.acquire("/plugin-a", "/tmp/native-memory-project");
    expect(replacement.client).not.toBe(first.client);
    expect(clients).toHaveLength(2);
    await replacement.release();
  });

  test("keeps different project roots independent", async () => {
    const clients: TrackingClient[] = [];
    const pool = new NativeMemoryClientPool(() => {
      const client = new TrackingClient();
      clients.push(client);
      return client;
    });

    const first = await pool.acquire("/plugin", "/tmp/native-memory-project-a");
    const second = await pool.acquire("/plugin", "/tmp/native-memory-project-b");

    expect(first.client).not.toBe(second.client);
    expect(clients).toHaveLength(2);

    await first.release();
    await second.release();
    expect(clients.map((client) => client.disposeCalls)).toEqual([1, 1]);
  });

  test("waits for final disposal before creating a replacement", async () => {
    const clients: TrackingClient[] = [];
    const pool = new NativeMemoryClientPool(() => {
      const client = new TrackingClient(clients.length > 0);
      clients.push(client);
      return client;
    });

    const first = await pool.acquire("/plugin", "/tmp/native-memory-project");
    const closing = first.release();
    await Promise.resolve();

    let acquired = false;
    const replacementPromise = pool
      .acquire("/plugin", "/tmp/native-memory-project")
      .then((lease) => {
        acquired = true;
        return lease;
      });
    await Promise.resolve();

    expect(acquired).toBe(false);
    expect(clients).toHaveLength(1);

    clients[0]?.finish();
    await closing;
    const replacement = await replacementPromise;
    expect(clients).toHaveLength(2);
    await replacement.release();
  });
});
