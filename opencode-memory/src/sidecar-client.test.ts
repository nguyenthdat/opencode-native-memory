import { PassThrough } from "node:stream";
import { EventEmitter } from "node:events";
import type { ChildProcessWithoutNullStreams } from "node:child_process";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import {
  NativeMemoryClient,
  NativeMemoryClientPool,
  REQUEST_TIMEOUT_MS,
  resolveNativeMemoryBinary,
} from "./sidecar-client.js";
import {
  RequestSchema,
  ResponseSchema,
  ValueObjectSchema,
  ValueSchema,
} from "./generated/opencode/memory/v1/memory_pb.js";
import { DelimitedFrameDecoder } from "./protocol.js";

test("bounds the configured native request timeout", () => {
  expect(REQUEST_TIMEOUT_MS).toBeGreaterThanOrEqual(1_000);
  expect(REQUEST_TIMEOUT_MS).toBeLessThanOrEqual(2 * 60 * 60_000);
});

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

describe("native memory client lifecycle", () => {
  test("terminates a timed-out sidecar before allowing a replacement", async () => {
    const children: FakeChild[] = [];
    const spawn = (_binary: string, _args: string[], _options: unknown) => {
      const child = new FakeChild(children.length > 0);
      children.push(child);
      return child.asChildProcess();
    };
    const client = new NativeMemoryClient(".", ".", spawn, 20);

    await expect(client.request("ingest", { path: "paper.pdf" })).rejects.toThrow(
      "Native memory ingest timed out after 20 ms",
    );
    expect(children).toHaveLength(1);
    expect(children[0]?.killSignals).toEqual(["SIGTERM"]);

    const replacement = client.request<{ rpc_protocol_version: number }>("status");
    await Promise.resolve();
    expect(children).toHaveLength(1);

    children[0]?.finishClose();
    await expect(replacement).resolves.toEqual({ rpc_protocol_version: 2 });
    expect(children).toHaveLength(2);
  });

  test("cleans up a sidecar that loses the writer lock during handshake", async () => {
    const child = new FakeChild(
      false,
      "another OpenCode process already owns this project's native memory writer lock",
    );
    const spawn = () => child.asChildProcess();
    const client = new NativeMemoryClient(".", ".", spawn, 20);

    await expect(client.request("status")).rejects.toThrow("another OpenCode process already owns");
    expect(child.killSignals).toEqual(["SIGTERM"]);

    await expect(client.request("status")).rejects.toThrow("another OpenCode process already owns");
    expect(child.killSignals).toEqual(["SIGTERM"]);
  });
});

class FakeChild extends EventEmitter {
  readonly stdin = new PassThrough();
  readonly stdout = new PassThrough();
  readonly stderr = new PassThrough();
  readonly pid = 999_999;
  readonly killSignals: NodeJS.Signals[] = [];
  exitCode: number | null = null;
  signalCode: NodeJS.Signals | null = null;
  private closed = false;

  constructor(
    private readonly respondToAll: boolean,
    private readonly handshakeError?: string,
  ) {
    super();
    const decoder = new DelimitedFrameDecoder(1024 * 1024);
    let responseCount = 0;
    this.stdin.on("data", (chunk: Buffer) => {
      for (const frame of decoder.push(chunk)) {
        const request = fromBinary(RequestSchema, frame);
        if (!this.respondToAll && responseCount > 0) continue;
        responseCount += 1;
        this.stdout.write(responseFrame(Number(request.id), this.handshakeError));
      }
    });
  }

  kill(signal?: NodeJS.Signals): boolean {
    this.killSignals.push(signal ?? "SIGTERM");
    this.signalCode = signal ?? "SIGTERM";
    this.emit("exit", null, this.signalCode);
    return true;
  }

  finishClose(): void {
    if (this.closed) return;
    this.closed = true;
    this.emit("close", null, this.signalCode);
  }

  asChildProcess(): ChildProcessWithoutNullStreams {
    return this as unknown as ChildProcessWithoutNullStreams;
  }
}

function responseFrame(id: number, error?: string): Uint8Array {
  const result = create(ValueSchema, {
    kind: {
      case: "objectValue",
      value: create(ValueObjectSchema, {
        fields: {
          rpc_protocol_version: create(ValueSchema, {
            kind: { case: "unsignedValue", value: 2n },
          }),
        },
      }),
    },
  });
  const response =
    error === undefined
      ? create(ResponseSchema, { id: BigInt(id), ok: true, result })
      : create(ResponseSchema, { id: BigInt(id), ok: false, error });
  const payload = toBinary(ResponseSchema, response);
  const frame: number[] = [];
  let length = payload.byteLength;
  while (length >= 0x80) {
    frame.push((length & 0x7f) | 0x80);
    length >>>= 7;
  }
  frame.push(length);
  return Uint8Array.from([...frame, ...payload]);
}
