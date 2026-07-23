import { Method } from "./generated/memory_pb.js";
import type { RpcResponse } from "./contracts.js";
declare const METHODS: {
    readonly search: Method.SEARCH;
    readonly store: Method.STORE;
    readonly get: Method.GET;
    readonly list: Method.LIST;
    readonly update: Method.UPDATE;
    readonly pin: Method.PIN;
    readonly lock: Method.LOCK;
    readonly delete: Method.DELETE;
    readonly forget: Method.FORGET;
    readonly purge: Method.PURGE;
    readonly feedback: Method.FEEDBACK;
    readonly sync_shared: Method.SYNC_SHARED;
    readonly status: Method.STATUS;
    readonly optimize: Method.OPTIMIZE;
    readonly doctor: Method.DOCTOR;
    readonly shutdown: Method.SHUTDOWN;
};
export type MemoryMethod = keyof typeof METHODS;
export declare function encodeRequest(id: number, method: string, params: unknown): Uint8Array;
export declare function decodeResponse(payload: Uint8Array): RpcResponse;
export declare class DelimitedFrameDecoder {
    private readonly maxFrameBytes;
    private buffered;
    constructor(maxFrameBytes: number);
    push(chunk: Uint8Array): Uint8Array[];
}
export {};
//# sourceMappingURL=protocol.d.ts.map