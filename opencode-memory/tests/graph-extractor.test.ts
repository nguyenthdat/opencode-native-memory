import { describe, expect, test } from "bun:test";
import {
  createGraphExtractor,
  validateGraphExtractionCandidates,
  type GraphExtractionCandidates,
  type GraphExtractionClient,
  type GraphRequestOptions,
  type GraphSessionCreateParameters,
  type GraphSessionPromptParameters,
  type GraphSdkResponse,
} from "../src/graph-extractor.js";

const validCandidates: GraphExtractionCandidates = {
  entities: [
    {
      mention: "OpenCode Memory",
      canonical_hint: "opencode memory",
      entity_type: "technology",
      aliases: ["native memory"],
      evidence: [{ source_unit_id: "unit-1", quote: "OpenCode Memory uses zvec." }],
      confidence: 0.96,
    },
  ],
  relations: [
    {
      subject_mention: "OpenCode Memory",
      predicate: "uses",
      object_mention: "zvec",
      relation_type: "technology_dependency",
      valid_at: null,
      evidence: [{ source_unit_id: "unit-1", quote: "OpenCode Memory uses zvec." }],
      confidence: 0.91,
    },
  ],
};

class FakeGraphClient implements GraphExtractionClient {
  readonly createCalls: Array<{
    parameters: GraphSessionCreateParameters;
    options: GraphRequestOptions | undefined;
  }> = [];
  readonly promptCalls: Array<{
    parameters: GraphSessionPromptParameters;
    options: GraphRequestOptions | undefined;
  }> = [];
  readonly deleteCalls: string[] = [];
  createResult: GraphSdkResponse<{ id: string }> = { data: { id: "graph-session" } };
  promptResult: GraphSdkResponse<{ info: { structured?: unknown; error?: unknown } }> = {
    data: { info: { structured: validCandidates } },
  };
  promptError: Error | undefined;
  onPrompt: (() => void) | undefined;

  readonly session = {
    create: async (
      parameters: GraphSessionCreateParameters,
      options?: GraphRequestOptions,
    ): Promise<GraphSdkResponse<{ id: string }>> => {
      this.createCalls.push({ parameters, options });
      return this.createResult;
    },
    prompt: async (
      parameters: GraphSessionPromptParameters,
      options?: GraphRequestOptions,
    ): Promise<GraphSdkResponse<{ info: { structured?: unknown; error?: unknown } }>> => {
      this.promptCalls.push({ parameters, options });
      this.onPrompt?.();
      if (this.promptError) throw this.promptError;
      return this.promptResult;
    },
    delete: async (
      parameters: { sessionID: string; directory: string },
      _options?: GraphRequestOptions,
    ): Promise<GraphSdkResponse<boolean>> => {
      this.deleteCalls.push(parameters.sessionID);
      return { data: true };
    },
  };
}

function createExtractor(
  client: GraphExtractionClient,
  retryCount = 2,
  onSessionChange?: (sessionID: string, active: boolean) => void,
) {
  return createGraphExtractor(
    {
      serverUrl: new URL("http://127.0.0.1:4096"),
      directory: "/tmp/graph-project",
    },
    {
      providerID: "anthropic",
      modelID: "claude-sonnet",
      retryCount,
      client,
      ...(onSessionChange === undefined ? {} : { onSessionChange }),
    },
  );
}

describe("OpenCode graph extractor", () => {
  test("uses an isolated denied session and returns validated candidates", async () => {
    const client = new FakeGraphClient();
    const sessionChanges: Array<[string, boolean]> = [];
    const extractor = createExtractor(client, 2, (sessionID, active) => {
      sessionChanges.push([sessionID, active]);
    });

    const result = await extractor.extract([
      { source_unit_id: "unit-1", text: "OpenCode Memory uses zvec." },
    ]);

    expect(result).toEqual(validCandidates);
    expect(client.createCalls).toHaveLength(1);
    expect(client.createCalls[0]?.parameters).toMatchObject({
      directory: "/tmp/graph-project",
      title: "OpenCode Memory graph extraction",
      model: { id: "claude-sonnet", providerID: "anthropic" },
      metadata: {
        purpose: "opencode-memory.graph-extraction",
        graph_extraction: true,
      },
      permission: [{ permission: "*", pattern: "*", action: "deny" }],
    });
    expect(client.promptCalls[0]?.parameters).toMatchObject({
      sessionID: "graph-session",
      directory: "/tmp/graph-project",
      model: { providerID: "anthropic", modelID: "claude-sonnet" },
      tools: { "*": false },
      format: { type: "json_schema", retryCount: 2 },
    });
    expect(client.promptCalls[0]?.parameters.system).toContain("untrusted evidence");
    expect(client.promptCalls[0]?.options?.signal).toBeInstanceOf(AbortSignal);
    expect(client.deleteCalls).toEqual(["graph-session"]);
    expect(sessionChanges).toEqual([
      ["graph-session", true],
      ["graph-session", false],
    ]);
  });

  test("rejects malformed structured output and deletes the session", async () => {
    const client = new FakeGraphClient();
    client.promptResult = { data: { info: { structured: { entities: "bad", relations: [] } } } };

    await expect(
      createExtractor(client).extract([{ source_unit_id: "unit-1", text: "evidence" }]),
    ).rejects.toThrow("entities must be an array");
    expect(client.deleteCalls).toEqual(["graph-session"]);
  });

  test("propagates provider errors and deletes the session", async () => {
    const client = new FakeGraphClient();
    client.promptError = new Error("provider unavailable");

    await expect(
      createExtractor(client).extract([{ source_unit_id: "unit-1", text: "evidence" }]),
    ).rejects.toThrow("provider unavailable");
    expect(client.deleteCalls).toEqual(["graph-session"]);
  });

  test("honors cancellation before and after the provider call", async () => {
    const beforeClient = new FakeGraphClient();
    const alreadyAborted = new AbortController();
    alreadyAborted.abort();
    await expect(
      createExtractor(beforeClient).extract(
        [{ source_unit_id: "unit-1", text: "evidence" }],
        alreadyAborted.signal,
      ),
    ).rejects.toHaveProperty("name", "AbortError");
    expect(beforeClient.createCalls).toHaveLength(0);

    const afterClient = new FakeGraphClient();
    const afterProvider = new AbortController();
    afterClient.onPrompt = () => afterProvider.abort();
    await expect(
      createExtractor(afterClient).extract(
        [{ source_unit_id: "unit-1", text: "evidence" }],
        afterProvider.signal,
      ),
    ).rejects.toHaveProperty("name", "AbortError");
    expect(afterClient.deleteCalls).toEqual(["graph-session"]);
  });

  test("enforces candidate, name, quote, and total JSON limits", () => {
    expect(() =>
      validateGraphExtractionCandidates({
        entities: Array.from({ length: 65 }, () => validCandidates.entities[0]),
        relations: [],
      }),
    ).toThrow("at most 64");
    expect(() =>
      validateGraphExtractionCandidates({
        entities: [{ ...validCandidates.entities[0], mention: "n".repeat(513) }],
        relations: [],
      }),
    ).toThrow("at most 512 characters");
    expect(() =>
      validateGraphExtractionCandidates({
        entities: [
          {
            ...validCandidates.entities[0],
            evidence: [{ source_unit_id: "unit-1", quote: "q".repeat(1_025) }],
          },
        ],
        relations: [],
      }),
    ).toThrow("at most 1024 characters");
    expect(() =>
      validateGraphExtractionCandidates({
        entities: [
          {
            ...validCandidates.entities[0],
            evidence: Array.from({ length: 257 }, (_, index) => ({
              source_unit_id: `unit-${index}`,
              quote: "q".repeat(1_024),
            })),
          },
        ],
        relations: [],
      }),
    ).toThrow("must not exceed 262144 bytes");
  });

  test("bounds structured-output retries", () => {
    expect(() => createExtractor(new FakeGraphClient(), 4)).toThrow(
      "retryCount must be an integer between 0 and 3",
    );
  });
});
