import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import {
  Method,
  RequestSchema,
  ResponseSchema,
  ValueObjectSchema,
  ValueSchema,
} from "./generated/memory_pb.js";
import { decodeResponse, DelimitedFrameDecoder, encodeRequest } from "./protocol.js";

describe("Protobuf memory protocol", () => {
  test("encodes a typed request with length-delimited framing", () => {
    const frame = encodeRequest(7, "search", {
      query: "memory",
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

  test("rejects unknown methods before writing to the sidecar", () => {
    expect(() => encodeRequest(1, "unknown", {})).toThrow("Unknown memory method");
  });
});

function withLength(payload: Uint8Array): Uint8Array {
  if (payload.byteLength >= 128) throw new Error("test payload is too large");
  const frame = new Uint8Array(payload.byteLength + 1);
  frame[0] = payload.byteLength;
  frame.set(payload, 1);
  return frame;
}
