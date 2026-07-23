import { describe, expect, test } from "bun:test";
import type { PendingRecall, SearchResponse } from "./contracts.js";
import type { MemoryMethod } from "./protocol.js";
import { SessionContext } from "./session-context.js";
import { NativeMemoryClient } from "./sidecar-client.js";

class FeedbackClient extends NativeMemoryClient {
  readonly requests: Array<{ method: MemoryMethod; params: unknown }> = [];

  constructor(
    private readonly onRequest?: (request: { method: MemoryMethod; params: unknown }) => void,
  ) {
    super(".", ".");
  }

  override async request<T>(method: MemoryMethod, params: unknown = {}): Promise<T> {
    const request = { method, params };
    this.requests.push(request);
    this.onRequest?.(request);
    return {} as T;
  }
}

describe("SessionContext recall state", () => {
  test("discards an unresolved recall before opening its replacement", async () => {
    const native = new FeedbackClient();
    const session = createSession(native);
    session.pendingRecall.set("session", pending("old"));

    await session.openPendingRecall("session", pending("new"));

    expect(native.requests.map(feedbackEvent)).toEqual(["injected"]);
    expect(session.pendingRecall.get("session")).toEqual(pending("new"));
  });

  test("does not install a replacement after its recall generation becomes stale", async () => {
    const native = new FeedbackClient();
    const session = createSession(native);
    session.pendingRecall.set("session", pending("old"));

    const opened = await session.openPendingRecall("session", pending("new"), () => false);

    expect(opened).toBe(false);
    expect(native.requests.map(feedbackEvent)).toEqual([]);
    expect(session.pendingRecall.has("session")).toBe(false);
  });

  test("closes a pending recall that becomes stale while injection feedback is recorded", async () => {
    let current = true;
    const native = new FeedbackClient((request) => {
      if (feedbackEvent(request) === "injected") current = false;
    });
    const session = createSession(native);

    const opened = await session.openPendingRecall("session", pending("new"), () => current);

    expect(opened).toBe(false);
    expect(native.requests.map(feedbackEvent)).toEqual(["injected"]);
    expect(session.pendingRecall.has("session")).toBe(false);
  });

  test("coalesces concurrent automatic searches with the same session and key", async () => {
    const session = createSession(new FeedbackClient());
    let searches = 0;
    let complete: ((response: SearchResponse) => void) | undefined;
    const deferred = new Promise<SearchResponse>((resolve) => {
      complete = resolve;
    });
    const search = (): Promise<SearchResponse> => {
      searches += 1;
      return deferred;
    };

    const first = session.searchRecallOnce("session", "key", search);
    const second = session.searchRecallOnce("session", "key", search);
    expect(searches).toBe(1);
    complete?.(searchResponse());

    const [firstResponse, secondResponse] = await Promise.all([first, second]);
    expect(firstResponse).toBe(secondResponse);
  });

  test("increments generations and clears the selected recall cache", () => {
    const session = createSession(new FeedbackClient());
    session.recallCache.set("one", { key: "one", response: searchResponse() });
    session.recallCache.set("two", { key: "two", response: searchResponse() });
    const initialOne = session.recallGeneration("one");
    const initialTwo = session.recallGeneration("two");

    session.invalidateRecall("one");
    expect(session.recallGeneration("one")).not.toBe(initialOne);
    expect(session.recallGeneration("two")).toBe(initialTwo);
    expect(session.recallCache.has("one")).toBe(false);
    expect(session.recallCache.has("two")).toBe(true);

    session.invalidateRecall();
    expect(session.recallGeneration("two")).not.toBe(initialTwo);
    expect(session.recallCache.size).toBe(0);
  });
});

function createSession(native: NativeMemoryClient): SessionContext {
  return new SessionContext(native, async () => ({ data: undefined }), ".");
}

function pending(id: string): PendingRecall {
  return { retrievalID: `ret_${id}`, memoryIDs: [`mem_${id}`] };
}

function feedbackEvent(request: { method: MemoryMethod; params: unknown }): unknown {
  if (request.method !== "feedback" || !isObject(request.params)) return undefined;
  return request.params.event;
}

function searchResponse(): SearchResponse {
  return {
    query: "query",
    count: 0,
    candidates_considered: 0,
    budget_chars: 2_400,
    used_chars: 0,
    abstained: true,
    score_version: "test",
    warnings: [],
    memories: [],
  };
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
