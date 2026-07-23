import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  Method,
  RequestSchema,
  ResponseSchema,
  ValueListSchema,
  ValueObjectSchema,
  ValueSchema,
} from "./generated/memory_pb.js";
import type { Response, Value } from "./generated/memory_pb.js";
import type { RpcResponse } from "./contracts.js";

const MAX_VALUE_DEPTH = 64;

const METHODS = {
  search: Method.SEARCH,
  store: Method.STORE,
  get: Method.GET,
  list: Method.LIST,
  update: Method.UPDATE,
  pin: Method.PIN,
  lock: Method.LOCK,
  delete: Method.DELETE,
  forget: Method.FORGET,
  purge: Method.PURGE,
  feedback: Method.FEEDBACK,
  sync_shared: Method.SYNC_SHARED,
  status: Method.STATUS,
  optimize: Method.OPTIMIZE,
  doctor: Method.DOCTOR,
  shutdown: Method.SHUTDOWN,
} as const;

export type MemoryMethod = keyof typeof METHODS;

export function encodeRequest(id: number, method: string, params: unknown): Uint8Array {
  if (!Number.isSafeInteger(id) || id <= 0) {
    throw new Error(`Invalid memory request ID: ${id}`);
  }
  const methodValue = METHODS[method as MemoryMethod];
  if (methodValue === undefined) {
    throw new Error(`Unknown memory method: ${method}`);
  }
  const request = create(RequestSchema, {
    id: BigInt(id),
    method: methodValue,
    params: encodeValue(params, 0),
  });
  return encodeDelimited(toBinary(RequestSchema, request));
}

export function decodeResponse(payload: Uint8Array): RpcResponse {
  const response: Response = fromBinary(ResponseSchema, payload);
  const id = safeNumber(response.id, "response ID");
  return {
    id,
    ok: response.ok,
    result: response.result ? decodeValue(response.result, 0) : undefined,
    error: response.error || undefined,
  };
}

export class DelimitedFrameDecoder {
  private buffered = new Uint8Array(0);

  constructor(private readonly maxFrameBytes: number) {}

  push(chunk: Uint8Array): Uint8Array[] {
    if (chunk.byteLength === 0) return [];
    const combined = new Uint8Array(this.buffered.byteLength + chunk.byteLength);
    combined.set(this.buffered);
    combined.set(chunk, this.buffered.byteLength);
    this.buffered = combined;

    const frames: Uint8Array[] = [];
    let offset = 0;
    while (offset < this.buffered.byteLength) {
      const header = readVarint(this.buffered, offset);
      if (!header) break;
      if (header.value > this.maxFrameBytes) {
        throw new Error(
          `Memory response exceeds ${this.maxFrameBytes} bytes (declared ${header.value})`,
        );
      }
      const frameEnd = header.next + header.value;
      if (frameEnd > this.buffered.byteLength) break;
      frames.push(this.buffered.slice(header.next, frameEnd));
      offset = frameEnd;
    }
    if (offset > 0) this.buffered = this.buffered.slice(offset);
    if (this.buffered.byteLength > this.maxFrameBytes + 10) {
      throw new Error(`Memory response exceeds ${this.maxFrameBytes} bytes`);
    }
    return frames;
  }
}

function encodeValue(input: unknown, depth: number): Value {
  if (depth > MAX_VALUE_DEPTH) {
    throw new Error("Memory request value nesting exceeds limit");
  }
  if (input === null || input === undefined) {
    return create(ValueSchema, {
      kind: { case: "nullValue", value: true },
    });
  }
  if (typeof input === "boolean") {
    return create(ValueSchema, {
      kind: { case: "booleanValue", value: input },
    });
  }
  if (typeof input === "bigint") {
    return create(ValueSchema, {
      kind:
        input >= 0n
          ? { case: "unsignedValue", value: input }
          : { case: "signedValue", value: input },
    });
  }
  if (typeof input === "number") {
    if (!Number.isFinite(input)) {
      throw new Error("Memory request contains a non-finite number");
    }
    if (Number.isSafeInteger(input)) {
      return create(ValueSchema, {
        kind:
          input >= 0
            ? { case: "unsignedValue", value: BigInt(input) }
            : { case: "signedValue", value: BigInt(input) },
      });
    }
    return create(ValueSchema, {
      kind: { case: "floatValue", value: input },
    });
  }
  if (typeof input === "string") {
    return create(ValueSchema, {
      kind: { case: "textValue", value: input },
    });
  }
  if (Array.isArray(input)) {
    return create(ValueSchema, {
      kind: {
        case: "listValue",
        value: create(ValueListSchema, {
          values: input.map((value) => encodeValue(value, depth + 1)),
        }),
      },
    });
  }
  if (typeof input === "object") {
    const fields: Record<string, Value> = {};
    for (const [key, value] of Object.entries(input)) {
      if (value !== undefined) fields[key] = encodeValue(value, depth + 1);
    }
    return create(ValueSchema, {
      kind: {
        case: "objectValue",
        value: create(ValueObjectSchema, { fields }),
      },
    });
  }
  throw new Error(`Unsupported memory request value: ${typeof input}`);
}

function decodeValue(input: Value, depth: number): unknown {
  if (depth > MAX_VALUE_DEPTH) {
    throw new Error("Memory response value nesting exceeds limit");
  }
  switch (input.kind.case) {
    case "booleanValue":
    case "floatValue":
    case "textValue":
      return input.kind.value;
    case "signedValue":
    case "unsignedValue":
      return safeNumber(input.kind.value, "response integer");
    case "listValue":
      return input.kind.value.values.map((value) => decodeValue(value, depth + 1));
    case "objectValue":
      return Object.fromEntries(
        Object.entries(input.kind.value.fields).map(([key, value]) => [
          key,
          decodeValue(value, depth + 1),
        ]),
      );
    case "nullValue":
    case undefined:
      return null;
  }
}

function safeNumber(value: bigint, label: string): number {
  const number = Number(value);
  if (!Number.isSafeInteger(number)) {
    throw new Error(`Memory ${label} exceeds JavaScript's safe integer range`);
  }
  return number;
}

function encodeDelimited(payload: Uint8Array): Uint8Array {
  const header = encodeVarint(payload.byteLength);
  const frame = new Uint8Array(header.byteLength + payload.byteLength);
  frame.set(header);
  frame.set(payload, header.byteLength);
  return frame;
}

function encodeVarint(value: number): Uint8Array {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`Invalid Protobuf frame length: ${value}`);
  }
  const bytes: number[] = [];
  let remaining = value;
  do {
    let byte = remaining % 128;
    remaining = Math.floor(remaining / 128);
    if (remaining > 0) byte |= 0x80;
    bytes.push(byte);
  } while (remaining > 0);
  return Uint8Array.from(bytes);
}

function readVarint(
  bytes: Uint8Array,
  offset: number,
): { value: number; next: number } | undefined {
  let value = 0;
  let multiplier = 1;
  for (let index = offset; index < bytes.byteLength && index < offset + 10; index += 1) {
    const byte = bytes[index]!;
    value += (byte & 0x7f) * multiplier;
    if (!Number.isSafeInteger(value)) {
      throw new Error("Invalid Protobuf frame length");
    }
    if ((byte & 0x80) === 0) return { value, next: index + 1 };
    multiplier *= 128;
  }
  if (bytes.byteLength - offset >= 10) {
    throw new Error("Invalid Protobuf frame length");
  }
  return undefined;
}
