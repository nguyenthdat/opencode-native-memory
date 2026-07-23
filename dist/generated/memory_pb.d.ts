import type { GenEnum, GenFile, GenMessage } from "@bufbuild/protobuf/codegenv2";
import type { Message } from "@bufbuild/protobuf";
/**
 * Describes the file memory.proto.
 */
export declare const file_memory: GenFile;
/**
 * @generated from message opencode.memory.v1.ValueList
 */
export type ValueList = Message<"opencode.memory.v1.ValueList"> & {
    /**
     * @generated from field: repeated opencode.memory.v1.Value values = 1;
     */
    values: Value[];
};
/**
 * Describes the message opencode.memory.v1.ValueList.
 * Use `create(ValueListSchema)` to create a new message.
 */
export declare const ValueListSchema: GenMessage<ValueList>;
/**
 * @generated from message opencode.memory.v1.ValueObject
 */
export type ValueObject = Message<"opencode.memory.v1.ValueObject"> & {
    /**
     * @generated from field: map<string, opencode.memory.v1.Value> fields = 1;
     */
    fields: {
        [key: string]: Value;
    };
};
/**
 * Describes the message opencode.memory.v1.ValueObject.
 * Use `create(ValueObjectSchema)` to create a new message.
 */
export declare const ValueObjectSchema: GenMessage<ValueObject>;
/**
 * @generated from message opencode.memory.v1.Value
 */
export type Value = Message<"opencode.memory.v1.Value"> & {
    /**
     * @generated from oneof opencode.memory.v1.Value.kind
     */
    kind: {
        /**
         * @generated from field: bool boolean_value = 1;
         */
        value: boolean;
        case: "booleanValue";
    } | {
        /**
         * @generated from field: sint64 signed_value = 2;
         */
        value: bigint;
        case: "signedValue";
    } | {
        /**
         * @generated from field: uint64 unsigned_value = 3;
         */
        value: bigint;
        case: "unsignedValue";
    } | {
        /**
         * @generated from field: double float_value = 4;
         */
        value: number;
        case: "floatValue";
    } | {
        /**
         * @generated from field: string text_value = 5;
         */
        value: string;
        case: "textValue";
    } | {
        /**
         * @generated from field: opencode.memory.v1.ValueList list_value = 6;
         */
        value: ValueList;
        case: "listValue";
    } | {
        /**
         * @generated from field: opencode.memory.v1.ValueObject object_value = 7;
         */
        value: ValueObject;
        case: "objectValue";
    } | {
        /**
         * @generated from field: bool null_value = 8;
         */
        value: boolean;
        case: "nullValue";
    } | {
        case: undefined;
        value?: undefined;
    };
};
/**
 * Describes the message opencode.memory.v1.Value.
 * Use `create(ValueSchema)` to create a new message.
 */
export declare const ValueSchema: GenMessage<Value>;
/**
 * @generated from message opencode.memory.v1.Request
 */
export type Request = Message<"opencode.memory.v1.Request"> & {
    /**
     * @generated from field: uint64 id = 1;
     */
    id: bigint;
    /**
     * @generated from field: opencode.memory.v1.Method method = 2;
     */
    method: Method;
    /**
     * @generated from field: opencode.memory.v1.Value params = 3;
     */
    params?: Value | undefined;
};
/**
 * Describes the message opencode.memory.v1.Request.
 * Use `create(RequestSchema)` to create a new message.
 */
export declare const RequestSchema: GenMessage<Request>;
/**
 * @generated from message opencode.memory.v1.Response
 */
export type Response = Message<"opencode.memory.v1.Response"> & {
    /**
     * @generated from field: uint64 id = 1;
     */
    id: bigint;
    /**
     * @generated from field: bool ok = 2;
     */
    ok: boolean;
    /**
     * @generated from field: opencode.memory.v1.Value result = 3;
     */
    result?: Value | undefined;
    /**
     * @generated from field: string error = 4;
     */
    error: string;
};
/**
 * Describes the message opencode.memory.v1.Response.
 * Use `create(ResponseSchema)` to create a new message.
 */
export declare const ResponseSchema: GenMessage<Response>;
/**
 * @generated from enum opencode.memory.v1.Method
 */
export declare enum Method {
    /**
     * @generated from enum value: METHOD_UNSPECIFIED = 0;
     */
    UNSPECIFIED = 0,
    /**
     * @generated from enum value: METHOD_SEARCH = 1;
     */
    SEARCH = 1,
    /**
     * @generated from enum value: METHOD_STORE = 2;
     */
    STORE = 2,
    /**
     * @generated from enum value: METHOD_GET = 3;
     */
    GET = 3,
    /**
     * @generated from enum value: METHOD_LIST = 4;
     */
    LIST = 4,
    /**
     * @generated from enum value: METHOD_UPDATE = 5;
     */
    UPDATE = 5,
    /**
     * @generated from enum value: METHOD_PIN = 6;
     */
    PIN = 6,
    /**
     * @generated from enum value: METHOD_LOCK = 7;
     */
    LOCK = 7,
    /**
     * @generated from enum value: METHOD_DELETE = 8;
     */
    DELETE = 8,
    /**
     * @generated from enum value: METHOD_FORGET = 9;
     */
    FORGET = 9,
    /**
     * @generated from enum value: METHOD_PURGE = 10;
     */
    PURGE = 10,
    /**
     * @generated from enum value: METHOD_FEEDBACK = 11;
     */
    FEEDBACK = 11,
    /**
     * @generated from enum value: METHOD_SYNC_SHARED = 12;
     */
    SYNC_SHARED = 12,
    /**
     * @generated from enum value: METHOD_STATUS = 13;
     */
    STATUS = 13,
    /**
     * @generated from enum value: METHOD_OPTIMIZE = 14;
     */
    OPTIMIZE = 14,
    /**
     * @generated from enum value: METHOD_DOCTOR = 15;
     */
    DOCTOR = 15,
    /**
     * @generated from enum value: METHOD_SHUTDOWN = 16;
     */
    SHUTDOWN = 16
}
/**
 * Describes the enum opencode.memory.v1.Method.
 */
export declare const MethodSchema: GenEnum<Method>;
//# sourceMappingURL=memory_pb.d.ts.map