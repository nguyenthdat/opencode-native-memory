import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import {
  Method,
  RequestSchema,
  ResponseSchema,
  ValueObjectSchema,
  ValueSchema,
} from "./generated/opencode/memory/v1/memory_pb.js";
import {
  DaemonRequestSchema,
  DaemonResponseSchema,
  DaemonStatusCode,
  CancelCallRequestSchema,
  GetDaemonInfoRequestSchema,
} from "./generated/opencode/memory/daemon/v1/daemon_pb.js";
import {
  decodeResponse,
  DelimitedFrameDecoder,
  encodeDelimited,
  encodeRequest,
} from "./protocol.js";

describe("Protobuf memory protocol", () => {
  test("encodes a typed request with length-delimited framing", () => {
    const frame = encodeRequest(7, "search", {
      query: "memory",
      retrieval_mode: "lexical",
      max_results: 5,
      enabled: true,
    });
    const [payload] = new DelimitedFrameDecoder(1024).push(frame);
    expect(payload).toBeDefined();
    const request = fromBinary(RequestSchema, payload!);
    expect(request.id).toBe(7n);
    expect(request.method).toBe(Method.SEARCH);
    expect(request.params?.kind.case).toBe("objectValue");
  });

  test("decodes fragmented response frames", () => {
    const result = create(ValueSchema, {
      kind: {
        case: "objectValue",
        value: create(ValueObjectSchema, {
          fields: {
            ready: create(ValueSchema, {
              kind: { case: "booleanValue", value: true },
            }),
            version: create(ValueSchema, {
              kind: { case: "unsignedValue", value: 2n },
            }),
          },
        }),
      },
    });
    const payload = toBinary(ResponseSchema, create(ResponseSchema, { id: 9n, ok: true, result }));
    const frame = withLength(payload);
    const decoder = new DelimitedFrameDecoder(1024);
    expect(decoder.push(frame.slice(0, 2))).toEqual([]);
    const [decodedPayload] = decoder.push(frame.slice(2));
    expect(decodeResponse(decodedPayload!)).toEqual({
      id: 9,
      ok: true,
      result: { ready: true, version: 2 },
      error: undefined,
    });
  });

  test("rejects unknown methods before writing to the native transport", () => {
    expect(() => encodeRequest(1, "unknown", {})).toThrow("Unknown memory method");
  });

  test("rejects integers outside the symmetric JavaScript safe range", () => {
    expect(() => encodeRequest(1, "status", { value: Number.MAX_SAFE_INTEGER })).not.toThrow();
    expect(() => encodeRequest(1, "status", { value: 2 ** 53 })).toThrow("safe range");
    expect(() =>
      encodeRequest(1, "status", { value: BigInt(Number.MAX_SAFE_INTEGER) + 1n }),
    ).toThrow("safe range");
    expect(() => encodeRequest(1, "status", { value: -(2n ** 63n) })).toThrow("safe range");
    expect(() => encodeRequest(1, "status", { value: 2n ** 64n - 1n })).toThrow("safe range");
  });

  test("encodes document ingestion as its own method", () => {
    const frame = encodeRequest(3, "ingest", { path: "paper.pdf" });
    const [payload] = new DelimitedFrameDecoder(1024).push(frame);
    const request = fromBinary(RequestSchema, payload!);
    expect(request.method).toBe(Method.INGEST);
  });

  test("encodes automatic document indexing as its own method", () => {
    const frame = encodeRequest(4, "index_documents", { force: false });
    const [payload] = new DelimitedFrameDecoder(1024).push(frame);
    const request = fromBinary(RequestSchema, payload!);
    expect(request.method).toBe(Method.INDEX_DOCUMENTS);
  });

  test("encodes a versioned daemon envelope with opaque request IDs", () => {
    const request = create(DaemonRequestSchema, {
      requestId: "call-2^53-plus-one",
      protocolGeneration: 1,
      body: { case: "getDaemonInfo", value: create(GetDaemonInfoRequestSchema) },
    });
    const frame = encodeDelimited(toBinary(DaemonRequestSchema, request));
    const [payload] = new DelimitedFrameDecoder(1024).push(frame);
    const decoded = fromBinary(DaemonRequestSchema, payload!);
    expect(decoded.requestId).toBe("call-2^53-plus-one");
    expect(decoded.body.case).toBe("getDaemonInfo");
  });

  test("keeps daemon status codes separate from domain responses", () => {
    const response = create(DaemonResponseSchema, {
      requestId: "request-1",
      status: { code: DaemonStatusCode.OUTCOME_UNKNOWN, message: "ambiguous" },
    });
    const decoded = fromBinary(DaemonResponseSchema, toBinary(DaemonResponseSchema, response));
    expect(decoded.status?.code).toBe(DaemonStatusCode.OUTCOME_UNKNOWN);
    expect(decoded.body.case).toBeUndefined();
  });

  test("encodes cancellation as a distinct daemon control operation", () => {
    const request = create(DaemonRequestSchema, {
      requestId: "cancel-request",
      protocolGeneration: 1,
      body: {
        case: "cancelCall",
        value: create(CancelCallRequestSchema, {
          sessionId: "session-a",
          projectHandle: "project-a",
          leaseId: "lease-a",
          callId: "call-a",
        }),
      },
    });
    const decoded = fromBinary(DaemonRequestSchema, toBinary(DaemonRequestSchema, request));
    expect(decoded.body.case).toBe("cancelCall");
  });
});

function withLength(payload: Uint8Array): Uint8Array {
  if (payload.byteLength >= 128) throw new Error("test payload is too large");
  const frame = new Uint8Array(payload.byteLength + 1);
  frame[0] = payload.byteLength;
  frame.set(payload, 1);
  return frame;
}
