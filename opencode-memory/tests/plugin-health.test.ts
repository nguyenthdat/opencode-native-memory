import { describe, expect, test } from "bun:test";
import type { DocumentIndexResponse, NativeMemoryStatus } from "../src/contracts.js";
import { buildMemoryStatusResponse } from "../src/plugin-health.js";

const nativeStatus: NativeMemoryStatus = {
  ready: true,
  rpc_protocol_version: 2,
  backend: "zvec+llama.cpp",
  zvec_version: "0.1.0",
  embedding_model: "test-model",
  embedding_dimension: 2560,
  active_profile_id: "qwen3-text-4b-q4",
  active_generation_id: "legacy",
  switch_state: null,
  switch_id: null,
  target_profile_id: null,
  switch_fraction: null,
  dense_search_available: true,
  project_root: "/project",
  project_id: "project-id",
  collection_path: "/data/collection",
  document_count: 1,
  indexed_document_count: 1,
  state_schema_version: 4,
  metadata_count: 1,
  tombstone_count: 0,
  retrieval_count: 0,
  pending_upsert_count: 0,
  pending_delete_count: 0,
  indexes: [{ name: "embedding", completeness: 1 }],
  capabilities: ["test_v1"],
};

const documentIndex: DocumentIndexResponse = {
  discovered: 1,
  added: 0,
  updated: 0,
  unchanged: 1,
  removed: 0,
  rejected: 0,
  inserted_chunks: 0,
  updated_chunks: 0,
  removed_chunks: 0,
  rejections: [],
  warnings: [],
};

describe("memory plugin health", () => {
  test("reports healthy when the backend and optional synchronization succeed", () => {
    const response = buildMemoryStatusResponse(
      { status: "fulfilled", value: nativeStatus },
      { status: "fulfilled", value: undefined },
      { status: "fulfilled", value: documentIndex },
      123,
    );

    expect(response).toMatchObject({
      ready: true,
      project_id: "project-id",
      plugin_health: {
        status: "healthy",
        ready: true,
        checked_at_ms: 123,
        issues: [],
      },
    });
  });

  test("reports degraded without hiding backend status when optional synchronization fails", () => {
    const response = buildMemoryStatusResponse(
      { status: "fulfilled", value: nativeStatus },
      { status: "rejected", reason: new Error("shared memory rejected") },
      {
        status: "fulfilled",
        value: {
          ...documentIndex,
          rejected: 1,
          rejections: [{ path: "docs/bad.md", message: "invalid document" }],
          warnings: ["index is incomplete"],
        },
      },
      456,
    );

    expect(response).toMatchObject({
      backend: "zvec+llama.cpp",
      plugin_health: {
        status: "degraded",
        ready: true,
        issues: [
          { component: "shared_sync", message: "shared memory rejected" },
          { component: "document_index", message: "docs/bad.md: invalid document" },
          { component: "document_index", message: "index is incomplete" },
        ],
      },
    });
  });

  test("returns unavailable health data when the native backend cannot be reached", () => {
    const response = buildMemoryStatusResponse(
      { status: "rejected", reason: new Error("daemon unavailable") },
      { status: "fulfilled", value: undefined },
      { status: "fulfilled", value: undefined },
      789,
    );

    expect(response).toEqual({
      plugin_health: {
        status: "unavailable",
        ready: false,
        checked_at_ms: 789,
        issues: [{ component: "backend", message: "daemon unavailable" }],
      },
    });
  });

  test("treats an explicit native not-ready response as unavailable", () => {
    const response = buildMemoryStatusResponse(
      { status: "fulfilled", value: { ...nativeStatus, ready: false } },
      { status: "fulfilled", value: undefined },
      { status: "fulfilled", value: undefined },
      999,
    );

    expect(response.plugin_health).toEqual({
      status: "unavailable",
      ready: false,
      checked_at_ms: 999,
      issues: [{ component: "backend", message: "Native memory backend is not ready" }],
    });
  });

  test("reports degraded backend maintenance state without marking it unavailable", () => {
    const response = buildMemoryStatusResponse(
      {
        status: "fulfilled",
        value: {
          ...nativeStatus,
          indexes: [{ name: "embedding", completeness: 0.5 }],
          pending_upsert_count: 2,
          pending_delete_count: 1,
        },
      },
      { status: "fulfilled", value: undefined },
      { status: "fulfilled", value: undefined },
      1_000,
    );

    expect(response.plugin_health).toEqual({
      status: "degraded",
      ready: true,
      checked_at_ms: 1_000,
      issues: [
        { component: "backend", message: "Index embedding is 50.0% complete" },
        { component: "backend", message: "2 memory upserts are pending recovery" },
        { component: "backend", message: "1 memory delete is pending recovery" },
      ],
    });
  });

  test("uses the optimize action threshold for backend index health", () => {
    for (const [completeness, status] of [
      [0.979, "degraded"],
      [0.98, "degraded"],
      [0.99, "degraded"],
      [1, "healthy"],
    ] as const) {
      const response = buildMemoryStatusResponse(
        {
          status: "fulfilled",
          value: { ...nativeStatus, indexes: [{ name: "embedding", completeness }] },
        },
        { status: "fulfilled", value: undefined },
        { status: "fulfilled", value: undefined },
      );

      expect(response.plugin_health.status).toBe(status);
    }
  });
});
