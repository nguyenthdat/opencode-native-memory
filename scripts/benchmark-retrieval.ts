import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { NativeMemoryClient } from "../opencode-memory/src/sidecar-client.js";
import type { RetrievalMode, SearchResponse } from "../opencode-memory/src/contracts.js";
import {
  computeRetrievalMetrics,
  type QueryRanking,
  type RelevanceJudgment,
} from "../tests/benchmark/metrics.js";

type BenchmarkMode = "no-memory" | RetrievalMode;

interface Manifest {
  schema_version: number;
  corpus_id: string;
  seed: number;
  memories_file: string;
  queries_file: string;
  memories_sha256: string;
  queries_sha256: string;
  max_results: number;
  budget_chars: number;
  min_score: number;
}

interface MemoryFixture {
  fixture_id: string;
  title: string;
  content: string;
  kind: string;
  importance: number;
  confidence: number;
  tags: string[];
  taxonomy: string;
}

interface QueryFixture {
  query_id: string;
  text: string;
  category: string;
  language: string;
  answerable: boolean;
  relevance: RelevanceJudgment[];
}

interface StoreResponse {
  id: string;
}

interface StatusResponse {
  capabilities: string[];
  embedding_model: string;
  embedding_dimension: number;
}

const runtimeProcess = (
  globalThis as typeof globalThis & {
    process: { argv: string[]; env: NodeJS.ProcessEnv };
  }
).process;
const args = parseArgs(runtimeProcess.argv.slice(2));
const repositoryRoot = resolve(import.meta.dir, "..");
const corpusDirectory = resolve(repositoryRoot, args.corpus);
const manifest = await readJson<Manifest>(join(corpusDirectory, "manifest.json"));
if (manifest.schema_version !== 1) {
  throw new Error(`Unsupported benchmark schema version: ${manifest.schema_version}`);
}
const memories = await readJsonLines<MemoryFixture>(join(corpusDirectory, manifest.memories_file));
const queries = await readJsonLines<QueryFixture>(join(corpusDirectory, manifest.queries_file));
assertUnique(
  memories.map((memory) => memory.fixture_id),
  "memory fixture ID",
);
assertUnique(
  queries.map((query) => query.query_id),
  "query ID",
);
const memoriesHash = await sha256File(join(corpusDirectory, manifest.memories_file));
const queriesHash = await sha256File(join(corpusDirectory, manifest.queries_file));
if (memoriesHash !== manifest.memories_sha256 || queriesHash !== manifest.queries_sha256) {
  throw new Error("Benchmark corpus hash differs from the frozen manifest");
}

const temporaryRoot = await mkdtemp(join(tmpdir(), "ocnm-benchmark-"));
const previousDataDirectory = runtimeProcess.env.OPENCODE_MEMORY_DATA_DIR;
const modeResults: Record<string, unknown> = {};
try {
  if (args.modes.includes("no-memory")) {
    modeResults["no-memory"] = runNoMemory(queries);
  }
  const retrievalModes = args.modes.filter((mode): mode is RetrievalMode => mode !== "no-memory");
  if (retrievalModes.length > 0) {
    Object.assign(
      modeResults,
      await runRetrievalModes(retrievalModes, memories, queries, manifest, temporaryRoot),
    );
  }
} finally {
  if (previousDataDirectory === undefined) delete runtimeProcess.env.OPENCODE_MEMORY_DATA_DIR;
  else runtimeProcess.env.OPENCODE_MEMORY_DATA_DIR = previousDataDirectory;
  await rm(temporaryRoot, { recursive: true, force: true });
}

const artifact = {
  schema_version: 1,
  corpus_id: manifest.corpus_id,
  seed: manifest.seed,
  generated_at: new Date().toISOString(),
  corpus_hashes: {
    memories_sha256: memoriesHash,
    queries_sha256: queriesHash,
  },
  modes: modeResults,
};
const serialized = `${JSON.stringify(artifact, null, 2)}\n`;
await mkdir(dirname(args.output), { recursive: true });
await writeFile(args.output, serialized);
console.log(serialized.trimEnd());
console.log(`Benchmark artifact written to ${args.output}`);

async function runRetrievalModes(
  modes: readonly RetrievalMode[],
  memories: readonly MemoryFixture[],
  queries: readonly QueryFixture[],
  manifest: Manifest,
  temporaryRoot: string,
): Promise<Record<string, unknown>> {
  runtimeProcess.env.OPENCODE_MEMORY_DATA_DIR = join(temporaryRoot, "retrieval");
  const client = new NativeMemoryClient(repositoryRoot, repositoryRoot);
  try {
    const status = await client.request<StatusResponse>("status");
    if (!status.capabilities.includes("search_retrieval_modes_v1")) {
      throw new Error("Sidecar does not advertise search_retrieval_modes_v1");
    }
    const productionToFixture = new Map<string, string>();
    for (const memory of [...memories].sort((left, right) =>
      left.fixture_id.localeCompare(right.fixture_id),
    )) {
      const stored = await client.request<StoreResponse>("store", {
        content: memory.content,
        title: memory.title,
        kind: memory.kind,
        importance: memory.importance,
        confidence: memory.confidence,
        tags: memory.tags,
        taxonomy: memory.taxonomy,
        scope: "project",
        origin: "manual",
        source: `benchmark:${memory.fixture_id}`,
      });
      productionToFixture.set(stored.id, memory.fixture_id);
    }

    const results: Record<string, unknown> = {};
    for (const mode of modes) {
      results[mode] = await evaluateMode(
        client,
        mode,
        queries,
        manifest,
        productionToFixture,
        status,
      );
    }
    return results;
  } finally {
    await client.dispose();
  }
}

async function evaluateMode(
  client: NativeMemoryClient,
  mode: RetrievalMode,
  queries: readonly QueryFixture[],
  manifest: Manifest,
  productionToFixture: ReadonlyMap<string, string>,
  status: StatusResponse,
): Promise<unknown> {
  const rankings: QueryRanking[] = [];
  const queryResults = [];
  for (const query of queries) {
    const started = performance.now();
    const response = await client.request<SearchResponse>("search", {
      query: query.text,
      retrieval_mode: mode,
      max_results: manifest.max_results,
      budget_chars: manifest.budget_chars,
      min_score: manifest.min_score,
      track_feedback: false,
    });
    const latencyMs = performance.now() - started;
    if (response.retrieval_mode !== mode) {
      throw new Error(`Requested ${mode}, sidecar reported ${response.retrieval_mode}`);
    }
    if (response.warnings.length > 0) {
      throw new Error(`${mode} search degraded: ${response.warnings.join("; ")}`);
    }
    const returned = response.memories.map((memory) => {
      const fixture = productionToFixture.get(memory.id);
      if (!fixture) throw new Error(`Unknown benchmark memory ID: ${memory.id}`);
      return fixture;
    });
    rankings.push({ answerable: query.answerable, returned, relevance: query.relevance });
    queryResults.push({
      query_id: query.query_id,
      category: query.category,
      language: query.language,
      latency_ms: latencyMs,
      returned,
      scores: response.memories.map((memory) => memory.score ?? null),
      candidates_considered: response.candidates_considered,
      score_version: response.score_version,
    });
  }
  const latencies = queryResults.map((result) => result.latency_ms).sort((a, b) => a - b);
  return {
    metrics: computeRetrievalMetrics(rankings),
    latency_ms: { p50: percentile(latencies, 0.5), p95: percentile(latencies, 0.95) },
    model: status.embedding_model,
    embedding_dimension: status.embedding_dimension,
    queries: queryResults,
  };
}

function runNoMemory(queries: readonly QueryFixture[]): unknown {
  const rankings = queries.map((query) => ({
    answerable: query.answerable,
    returned: [],
    relevance: query.relevance,
  }));
  return {
    metrics: computeRetrievalMetrics(rankings),
    latency_ms: null,
    queries: queries.map((query) => ({ query_id: query.query_id, returned: [] })),
  };
}

function parseArgs(values: string[]): { corpus: string; modes: BenchmarkMode[]; output: string } {
  const options = new Map<string, string>();
  for (let index = 0; index < values.length; index += 2) {
    const key = values[index];
    const value = values[index + 1];
    if (!key?.startsWith("--") || value === undefined) {
      throw new Error("Usage: benchmark-retrieval --corpus <dir> --modes <list> --output <file>");
    }
    options.set(key.slice(2), value);
  }
  const allowed = new Set<BenchmarkMode>(["no-memory", "lexical", "dense", "hybrid"]);
  const modes = (options.get("modes") ?? "no-memory,lexical,dense,hybrid")
    .split(",")
    .map((mode) => mode.trim() as BenchmarkMode);
  if (modes.length === 0 || modes.some((mode) => !allowed.has(mode))) {
    throw new Error(`Invalid benchmark modes: ${modes.join(",")}`);
  }
  return {
    corpus: options.get("corpus") ?? "tests/benchmark/retrieval-v1",
    modes,
    output: resolve(options.get("output") ?? "target/benchmarks/retrieval-v1.json"),
  };
}

async function readJson<T>(path: string): Promise<T> {
  return JSON.parse(await readFile(path, "utf8")) as T;
}

async function readJsonLines<T>(path: string): Promise<T[]> {
  return (await readFile(path, "utf8"))
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => JSON.parse(line) as T);
}

function assertUnique(values: readonly string[], label: string): void {
  if (new Set(values).size !== values.length) throw new Error(`Duplicate ${label}`);
}

async function sha256File(path: string): Promise<string> {
  return createHash("sha256")
    .update(await readFile(path))
    .digest("hex");
}

function percentile(sorted: readonly number[], quantile: number): number {
  if (sorted.length === 0) return 0;
  const index = Math.min(sorted.length - 1, Math.ceil(sorted.length * quantile) - 1);
  return sorted[index] ?? 0;
}
