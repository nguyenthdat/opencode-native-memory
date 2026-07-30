import { createHash, randomUUID } from "node:crypto";
import type { Plugin, ToolResult } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type {
  MemoryRecord,
  ListResponse,
  SearchResponse,
  SharedSyncResponse,
  IngestResponse,
  DocumentIndexResponse,
  NativeMemoryStatus,
  GraphAuthorization as GraphAuthorizationContract,
  GraphSearchRequest,
  GraphSearchResponse,
  GraphStatusRequest,
  GraphStatusResponse,
  GraphExportRequest,
  GraphExportResponse,
  GraphExtractCancelResponse,
  GraphExtractClaimResponse,
  GraphExtractEnqueueResponse,
  GraphExtractFinishResponse,
  GraphExtractJobStatusResponse,
  GraphExtractRenewResponse,
  GraphExtractionJob,
  GraphExtractionUnit,
  GraphProviderIdentity,
  GraphSourceBinding,
} from "./contracts.js";
import {
  MEMORY_KINDS,
  MEMORY_SCOPES,
  WRITABLE_MEMORY_SCOPES,
  FEEDBACK_EVENTS,
  LOCK_ACTIONS,
  MEMORY_TAXONOMIES,
  RETRIEVAL_MODES,
  isUserProfileTaxonomy,
} from "./contracts.js";
import { acquireNativeMemoryClient, DaemonOutcomeUnknownError } from "./daemon-client.js";
import { createGraphExtractor, type GraphExtractionCandidates } from "./graph-extractor.js";
import {
  COMPACTION_CONTEXT,
  formatRecalledMemories,
  truncateText,
  contextBudgetChars,
  parseCuratedCandidates,
  deriveRecallQuery,
  extractDirectUserEvidence,
  hasDirectUserEvidence,
} from "./policy.js";
import {
  MEMORY_INSTRUCTIONS_MARKER,
  loadMemoryInstructions,
  registerMemoryInstructions,
} from "./instructions.js";
import {
  SHARED_MEMORY_RELATIVE_DIR,
  loadSharedMemories,
  writeSharedMemory,
} from "./shared-markdown.js";
import { SessionContext } from "./session-context.js";
import { validateDeleteRecords, validateUpdateArgs } from "./validation.js";
import { BACKGROUND_JOB_ID_PATTERN, BackgroundJobQueue } from "./background-jobs.js";
import { buildMemoryStatusResponse } from "./plugin-health.js";
import {
  cancelModelSwitch,
  getModelSwitchStatus,
  listModelProfiles,
  startModelSwitch,
} from "./model-control.js";
import { DEFAULT_OPTIMIZE_DEBOUNCE_MS, MemoryMaintenanceScheduler } from "./maintenance.js";
import { requestIdempotently } from "./outcome-reconciliation.js";
import { captureWithOutcomeReconciliation } from "./capture-reconciliation.js";

export interface MemoryPluginOptions {
  root: string;
  warmup?: boolean;
  automaticRecall?: boolean;
  automaticCapture?: boolean;
  automaticDocumentIndex?: boolean;
  documentIndexDebounceMs?: number;
  automaticOptimize?: boolean;
  optimizeDebounceMs?: number;
  sharedSync?: boolean;
  feedbackTracking?: boolean;
  minScore?: number;
  projectRoot?: string;
}

type GraphPreparedUnit = GraphExtractionUnit;

interface GraphPrepareResponse {
  readonly units: readonly GraphPreparedUnit[];
  readonly rejected_sources: readonly unknown[];
  readonly warnings: readonly string[];
}

const GRAPH_JOB_LEASE_MS = 60_000;
const GRAPH_JOB_RENEW_INTERVAL_MS = 20_000;
const GRAPH_JOB_POLL_MS = 1_000;
const GRAPH_EXTRACTOR_VERSION = "opencode-sdk-v2-json-schema-v1";
const GRAPH_PROMPT_VERSION = "knowledge-graph-extraction-v1";
const GRAPH_CANDIDATE_SCHEMA_VERSION = "knowledge-graph-candidates-v1";

export function createMemoryPlugin(options: MemoryPluginOptions): Plugin {
  return async ({ client: opencode, directory, worktree, serverUrl }) => {
    const settings = resolveMemoryPluginOptions(options);
    const memoryProjectRoot = options.projectRoot ?? worktree;
    const memoryInstructions = await loadMemoryInstructions(options.root);
    const nativeLease = await acquireNativeMemoryClient(options.root, memoryProjectRoot);
    const native = nativeLease.client;
    const session = new SessionContext(
      native,
      (path, query) => opencode.session.get({ path, query }),
      directory,
    );
    const maintenance = new MemoryMaintenanceScheduler(native, {
      enabled: settings.automaticOptimize,
      debounceMs: settings.optimizeDebounceMs,
      onError: session.warnOnce,
    });
    let sharedSignature: string | undefined;
    let sharedSync: Promise<void> | undefined;
    let documentSync: Promise<DocumentIndexResponse> | undefined;
    let documentSyncTimer: ReturnType<typeof setTimeout> | undefined;
    let deferredProjectSync = false;
    let deferredSwitchDiscovery: Promise<void> | undefined;
    type IngestJobInput = {
      path: string;
      scope: (typeof WRITABLE_MEMORY_SCOPES)[number];
    };
    const ingestJobs = new BackgroundJobQueue<IngestJobInput, IngestResponse>();
    const extractionSessions = new Set<string>();
    const graphModels = new Map<
      string,
      { providerID: string; modelID: string; variant?: string }
    >();

    const graphAuthorization = async (
      sessionID: string,
      agent: string,
    ): Promise<GraphAuthorizationContract> => ({
      session_scope_key: await session.resolveSessionRoot(sessionID),
      agent_scope_key: agent,
    });

    const graphWorkerID = `plugin-${randomUUID()}`;
    const graphWorkerAbort = new AbortController();
    const graphWork = new Map<string, GraphAuthorizationContract>();
    const graphDiscoveries = new Map<string, GraphAuthorizationContract>();
    const graphAttemptAborts = new Map<string, AbortController>();
    const modelSwitchMonitors = new Set<string>();
    let graphWorker: Promise<void> | undefined;

    const graphJobStatus = async (
      jobID: string,
      authorization: GraphAuthorizationContract,
      signal: AbortSignal,
    ): Promise<GraphExtractJobStatusResponse> =>
      await native.request<GraphExtractJobStatusResponse>(
        "graph_extract_job_status",
        { authorization, job_id: jobID },
        signal,
      );

    const reconcileGraphFinish = async (
      method: "graph_extract_complete" | "graph_extract_fail",
      params: Record<string, unknown>,
      jobID: string,
      authorization: GraphAuthorizationContract,
      signal: AbortSignal,
    ): Promise<GraphExtractFinishResponse | undefined> => {
      try {
        return await native.request<GraphExtractFinishResponse>(method, params, signal);
      } catch (error) {
        if (!(error instanceof DaemonOutcomeUnknownError)) throw error;
        const status = await graphJobStatus(jobID, authorization, signal);
        if (!status.found || !status.job) throw error;
        if (["completed", "failed", "cancelled", "queued"].includes(status.job.state)) {
          return undefined;
        }
        return await native.request<GraphExtractFinishResponse>(method, params, signal);
      }
    };

    const claimGraphJob = async (
      jobID: string | undefined,
      authorization: GraphAuthorizationContract,
    ): Promise<GraphExtractClaimResponse> =>
      await native.request<GraphExtractClaimResponse>(
        "graph_extract_claim",
        {
          authorization,
          claim_request_id: randomUUID(),
          worker_id: graphWorkerID,
          ...(jobID === undefined ? {} : { job_id: jobID }),
          lease_ttl_ms: GRAPH_JOB_LEASE_MS,
        },
        graphWorkerAbort.signal,
      );

    const processClaimedGraphJob = async (
      claim: GraphExtractClaimResponse,
      authorization: GraphAuthorizationContract,
    ): Promise<void> => {
      if (!claim.found || !claim.job || !claim.lease_token) return;
      if (claim.job.state === "failed" || claim.job.state === "cancelled") return;
      const provider = claim.job.provider;
      if (!provider) throw new Error("Claimed graph job omitted its provider identity");
      const units = claim.units.filter(
        (unit): unit is GraphExtractionUnit & { source: GraphSourceBinding } =>
          unit.source?.remote_eligible === true,
      );
      if (units.length === 0) {
        await reconcileGraphFinish(
          "graph_extract_fail",
          {
            authorization,
            job_id: claim.job.job_id,
            lease_token: claim.lease_token,
            extraction_run_id: claim.job.extraction_run_id,
            retryable: false,
            error_code: "no_eligible_sources",
            error_message: "claimed graph job has no remotely eligible source units",
          },
          claim.job.job_id,
          authorization,
          graphWorkerAbort.signal,
        );
        return;
      }

      const attemptAbort = new AbortController();
      graphAttemptAborts.set(claim.job.job_id, attemptAbort);
      const attemptSignal = AbortSignal.any([graphWorkerAbort.signal, attemptAbort.signal]);
      let renewalFailure: Error | undefined;
      let renewal = Promise.resolve();
      const renewLease = async (): Promise<void> => {
        const response = await native.request<GraphExtractRenewResponse>(
          "graph_extract_renew",
          {
            authorization,
            job_id: claim.job!.job_id,
            lease_token: claim.lease_token,
            lease_ttl_ms: GRAPH_JOB_LEASE_MS,
          },
          graphWorkerAbort.signal,
        );
        if (response.cancel_requested) {
          attemptAbort.abort(new Error("Graph extraction was cancelled"));
        }
      };
      await renewLease();
      const renewalTimer = setInterval(() => {
        renewal = renewal.then(renewLease).catch((error: unknown) => {
          renewalFailure = asPluginError(error);
          attemptAbort.abort(renewalFailure);
        });
      }, GRAPH_JOB_RENEW_INTERVAL_MS);
      renewalTimer.unref?.();
      attemptAbort.signal.addEventListener("abort", () => clearInterval(renewalTimer), {
        once: true,
      });

      let candidates: GraphExtractionCandidates | undefined;
      let candidatePayload: ReturnType<typeof graphCandidatePayload> | undefined;
      let extractionFailure: Error | undefined;
      try {
        const extractor = createGraphExtractor(
          { serverUrl, directory },
          {
            providerID: provider.provider_id,
            modelID: provider.model_id,
            ...(provider.variant === undefined ? {} : { variant: provider.variant }),
            retryCount: 0,
            onSessionChange: (sessionID, active) => {
              if (active) extractionSessions.add(sessionID);
              else extractionSessions.delete(sessionID);
            },
          },
        );
        candidates = await extractor.extract(
          units.map((unit) => ({
            source_unit_id: unit.source.source_unit_id,
            text: unit.text,
          })),
          attemptSignal,
        );
        candidatePayload = graphCandidatePayload(candidates);
      } catch (error) {
        extractionFailure = asPluginError(error);
      } finally {
        clearInterval(renewalTimer);
        await renewal;
      }
      if (graphAttemptAborts.get(claim.job.job_id) === attemptAbort) {
        graphAttemptAborts.delete(claim.job.job_id);
      }
      if (graphWorkerAbort.signal.aborted || renewalFailure) return;
      if (!candidates || !candidatePayload || extractionFailure) {
        const cancelled = attemptAbort.signal.aborted;
        await reconcileGraphFinish(
          "graph_extract_fail",
          {
            authorization,
            job_id: claim.job.job_id,
            lease_token: claim.lease_token,
            extraction_run_id: claim.job.extraction_run_id,
            retryable: !cancelled,
            error_code: cancelled ? "cancelled" : "provider_error",
            error_message: boundedGraphJobError(
              extractionFailure?.message ?? "graph provider extraction failed",
            ),
          },
          claim.job.job_id,
          authorization,
          graphWorkerAbort.signal,
        );
        return;
      }
      await reconcileGraphFinish(
        "graph_extract_complete",
        {
          authorization,
          job_id: claim.job.job_id,
          lease_token: claim.lease_token,
          extraction_run_id: claim.job.extraction_run_id,
          ...candidatePayload,
        },
        claim.job.job_id,
        authorization,
        graphWorkerAbort.signal,
      );
    };

    const processGraphJob = async (
      job: GraphExtractionJob,
      authorization: GraphAuthorizationContract,
    ): Promise<void> => {
      const claim = await claimGraphJob(job.job_id, authorization);
      await processClaimedGraphJob(claim, authorization);
    };

    const runGraphWorker = async (): Promise<void> => {
      while (
        !graphWorkerAbort.signal.aborted &&
        (graphWork.size > 0 || graphDiscoveries.size > 0)
      ) {
        let progressed = false;
        for (const [key, authorization] of [...graphDiscoveries]) {
          if (graphWorkerAbort.signal.aborted) return;
          try {
            const claim = await claimGraphJob(undefined, authorization);
            if (!claim.found || !claim.job) {
              const aggregate = await native.request<GraphStatusResponse>(
                "graph_status",
                { authorization },
                graphWorkerAbort.signal,
              );
              if (aggregate.pending_job_count === 0) graphDiscoveries.delete(key);
              continue;
            }
            progressed = true;
            graphWork.set(claim.job.job_id, authorization);
            await processClaimedGraphJob(claim, authorization);
          } catch (error) {
            if (!graphWorkerAbort.signal.aborted) session.warnOnce(error);
          }
        }
        for (const [jobID, authorization] of [...graphWork]) {
          if (graphWorkerAbort.signal.aborted) return;
          try {
            const status = await graphJobStatus(jobID, authorization, graphWorkerAbort.signal);
            if (!status.found || !status.job || status.job.state === "completed") {
              graphWork.delete(jobID);
              continue;
            }
            if (status.job.state === "failed" || status.job.state === "cancelled") {
              graphWork.delete(jobID);
              if (status.job.error_message) session.warnOnce(new Error(status.job.error_message));
              continue;
            }
            if (
              status.job.state === "queued" &&
              (status.job.next_attempt_at_ms === undefined ||
                status.job.next_attempt_at_ms <= Date.now())
            ) {
              progressed = true;
              await processGraphJob(status.job, authorization);
            }
          } catch (error) {
            if (!graphWorkerAbort.signal.aborted) session.warnOnce(error);
          }
        }
        if (
          !progressed &&
          !graphWorkerAbort.signal.aborted &&
          (graphWork.size > 0 || graphDiscoveries.size > 0)
        ) {
          await graphWorkerDelay(GRAPH_JOB_POLL_MS, graphWorkerAbort.signal);
        }
      }
    };

    const startGraphWorker = (): void => {
      if (
        graphWorker ||
        graphWorkerAbort.signal.aborted ||
        (graphWork.size === 0 && graphDiscoveries.size === 0)
      ) {
        return;
      }
      const running = runGraphWorker();
      const tracked = running.finally(() => {
        if (graphWorker === tracked) graphWorker = undefined;
        startGraphWorker();
      });
      graphWorker = tracked;
    };

    const scheduleGraphJob = (jobID: string, authorization: GraphAuthorizationContract): void => {
      if (graphWorkerAbort.signal.aborted) return;
      graphWork.set(jobID, authorization);
      startGraphWorker();
    };

    const scheduleGraphDiscovery = (authorization: GraphAuthorizationContract): void => {
      if (graphWorkerAbort.signal.aborted) return;
      graphDiscoveries.set(graphAuthorizationKey(authorization), authorization);
      startGraphWorker();
    };

    const syncSharedMemories = async (force = false): Promise<void> => {
      if (!settings.sharedSync) return;
      if (sharedSync) return await sharedSync;
      sharedSync = (async () => {
        const loaded = await loadSharedMemories(memoryProjectRoot);
        for (const error of loaded.errors) {
          session.warnOnce(new Error(`${error.source}: ${error.message}`));
        }
        if (!force && loaded.signature === sharedSignature) return;
        const { response } = await requestIdempotently<SharedSyncResponse>(native, "sync_shared", {
          records: loaded.records,
        });
        if (response.rejected > 0) {
          throw new Error(
            `Rejected shared memories: ${response.rejections
              .map((rejection) => `${rejection.source}: ${rejection.message}`)
              .join(", ")}`,
          );
        }
        sharedSignature = loaded.signature;
        session.invalidateRecall();
        if (response.imported > 0 || response.removed > 0) maintenance.schedule();
      })()
        .catch((error: unknown) => {
          if (isModelSwitchInProgressError(error)) {
            deferredProjectSync = true;
            scheduleDeferredSwitchMonitor();
          }
          throw error;
        })
        .finally(() => {
          sharedSync = undefined;
        });
      await sharedSync;
    };

    const indexDocuments = async (force = false): Promise<DocumentIndexResponse> => {
      if (documentSync) return await documentSync;
      documentSync = requestIdempotently<DocumentIndexResponse>(native, "index_documents", {
        force,
      })
        .then(({ response, reconciled }) => {
          if (reconciled) maintenance.schedule();
          return response;
        })
        .then((response) => {
          for (const rejection of response.rejections) {
            session.warnOnce(new Error(`${rejection.path}: ${rejection.message}`));
          }
          for (const warning of response.warnings) session.warnOnce(new Error(warning));
          if (response.added > 0 || response.updated > 0 || response.removed > 0) {
            session.invalidateRecall();
            maintenance.schedule();
          }
          return response;
        })
        .catch((error: unknown) => {
          if (isModelSwitchInProgressError(error)) {
            deferredProjectSync = true;
            scheduleDeferredSwitchMonitor();
          }
          throw error;
        })
        .finally(() => {
          documentSync = undefined;
        });
      return await documentSync;
    };

    const syncDocuments = async (): Promise<void> => {
      if (!settings.automaticDocumentIndex) return;
      await indexDocuments();
    };

    const syncProjectIndexes = async (): Promise<void> => {
      await Promise.all([syncSharedMemories(), syncDocuments()]);
    };

    const flushDeferredProjectSync = (): void => {
      if (!deferredProjectSync) return;
      deferredProjectSync = false;
      void syncProjectIndexes().catch(session.warnOnce);
    };

    const monitorModelSwitch = (switchID: string): void => {
      if (modelSwitchMonitors.has(switchID) || graphWorkerAbort.signal.aborted) return;
      modelSwitchMonitors.add(switchID);
      void (async () => {
        while (!graphWorkerAbort.signal.aborted) {
          let status: Awaited<ReturnType<typeof getModelSwitchStatus>>;
          try {
            status = await getModelSwitchStatus(native, switchID, graphWorkerAbort.signal);
          } catch (error) {
            if (graphWorkerAbort.signal.aborted) return;
            session.warnOnce(error);
            await graphWorkerDelay(1_000, graphWorkerAbort.signal);
            continue;
          }
          if (["succeeded", "cancelled", "failed"].includes(status.state)) {
            session.invalidateRecall();
            flushDeferredProjectSync();
            return;
          }
          await graphWorkerDelay(1_000, graphWorkerAbort.signal);
        }
      })()
        .catch((error: unknown) => {
          if (!graphWorkerAbort.signal.aborted) session.warnOnce(error);
        })
        .finally(() => modelSwitchMonitors.delete(switchID));
    };

    function scheduleDeferredSwitchMonitor(): void {
      if (deferredSwitchDiscovery || graphWorkerAbort.signal.aborted) return;
      deferredSwitchDiscovery = (async () => {
        while (deferredProjectSync && !graphWorkerAbort.signal.aborted) {
          try {
            const status = await native.request<NativeMemoryStatus>(
              "status",
              {},
              graphWorkerAbort.signal,
            );
            if (!status.switch_id || !status.switch_state) {
              flushDeferredProjectSync();
              return;
            }
            if (["succeeded", "cancelled", "failed"].includes(status.switch_state)) {
              session.invalidateRecall();
              flushDeferredProjectSync();
              return;
            }
            monitorModelSwitch(status.switch_id);
            return;
          } catch (error) {
            if (graphWorkerAbort.signal.aborted) return;
            session.warnOnce(error);
            await graphWorkerDelay(1_000, graphWorkerAbort.signal);
          }
        }
      })().finally(() => {
        deferredSwitchDiscovery = undefined;
      });
    }

    const scheduleDocumentSync = (): void => {
      if (!settings.automaticDocumentIndex) return;
      if (documentSyncTimer) clearTimeout(documentSyncTimer);
      documentSyncTimer = setTimeout(() => {
        documentSyncTimer = undefined;
        void syncDocuments().catch(session.warnOnce);
      }, settings.documentIndexDebounceMs);
    };

    if (settings.warmup || settings.automaticDocumentIndex) {
      const startup = settings.warmup
        ? [
            native
              .request<NativeMemoryStatus>("status")
              .then((status) => maintenance.observeStatus(status)),
            syncProjectIndexes(),
          ]
        : [syncDocuments()];
      void Promise.all(startup).catch(session.warnOnce);
    }

    return {
      dispose: async () => {
        if (documentSyncTimer) clearTimeout(documentSyncTimer);
        graphWorkerAbort.abort();
        if (deferredSwitchDiscovery) await deferredSwitchDiscovery.catch(() => undefined);
        if (graphWorker) await graphWorker.catch(() => undefined);
        graphWork.clear();
        graphDiscoveries.clear();
        graphAttemptAborts.clear();
        modelSwitchMonitors.clear();
        await maintenance.dispose();
        await ingestJobs.dispose();
        if (sharedSync) await sharedSync.catch(() => undefined);
        if (documentSync) await documentSync.catch(() => undefined);
        for (const sessID of session.pendingRecall.keys()) session.discardPendingRecall(sessID);
        session.latestQuery.clear();
        session.invalidateRecall();
        session.pendingRecall.clear();
        session.sessionParents.clear();
        session.sessionRoots.clear();
        session.sessionAgents.clear();
        extractionSessions.clear();
        graphModels.clear();
        await nativeLease.release();
      },
      config: async (config) => {
        await registerMemoryInstructions(config, memoryInstructions, directory);
        config.command ??= {};
        config.command.memory ??= {
          description: "Inspect and manage project memory",
          template: `Manage memory for the current project. User request: $ARGUMENTS

When no arguments are supplied, call memory_status and memory_list, then summarize active scopes, stale/expired records, and suggested cleanup.
Use memory_search for semantic lookup, memory_get for full records, memory_update for corrections, memory_delete for removal, memory_promote for reviewed Git-shareable Markdown, and memory_doctor for diagnostics.

For model requests:
- "model profiles": call memory_model_profiles exactly once and distinguish stable, preview, and unsupported profiles.
- "model switch <profile>": call memory_model_switch exactly once, report the durable switch ID, and do not wait unless requested.
- "model status <switch-id>": call memory_model_switch_status.
- "model cancel <switch-id>": call memory_model_switch_cancel.
Never change embedding environment variables or restart the daemon to switch an existing project profile.

Never modify repository-scoped memory through memory_update; edit its .opencode/memory Markdown source instead. Ask through the tool permission flow before destructive or sharing operations.`,
        };
      },
      event: async ({ event }) => {
        if (event.type === "session.created" || event.type === "session.updated") {
          session.sessionParents.set(event.properties.info.id, event.properties.info.parentID);
          session.sessionRoots.clear();
          return;
        }
        if (event.type === "session.deleted") {
          const sessID = event.properties.info.id;
          session.discardPendingRecall(sessID);
          session.latestQuery.delete(sessID);
          session.invalidateRecall(sessID);
          session.sessionParents.delete(sessID);
          session.sessionRoots.delete(sessID);
          session.sessionAgents.delete(sessID);
          graphModels.delete(sessID);
          return;
        }
        if (event.type === "session.idle") {
          session.discardPendingRecall(event.properties.sessionID);
          return;
        }
        if (event.type === "session.error" && event.properties.sessionID) {
          await session.closePendingRecall(event.properties.sessionID, "error");
          return;
        }
        if (event.type === "file.edited" || event.type === "file.watcher.updated") {
          session.invalidateRecall();
          const file = event.properties.file.replaceAll("\\", "/");
          if (file.includes(`/${SHARED_MEMORY_RELATIVE_DIR}/`)) {
            sharedSignature = undefined;
          }
          if (isSupportedDocumentPath(file)) scheduleDocumentSync();
          return;
        }
        if (event.type !== "session.compacted") return;
        if (extractionSessions.has(event.properties.sessionID)) return;
        if (!settings.automaticCapture) return;

        try {
          const response = await opencode.session.messages({
            path: { id: event.properties.sessionID },
            query: { directory, limit: 50 },
          });
          const messages = response.data ?? [];
          const summary = messages
            ?.toReversed()
            .find((message) => message.info.role === "assistant" && message.info.summary === true);
          if (!summary) return;
          const content = summary.parts
            .flatMap((part) => (part.type === "text" && !part.ignored ? [part.text] : []))
            .join("\n")
            .trim();
          if (!content) return;
          const candidates = parseCuratedCandidates(content, extractDirectUserEvidence(messages));
          let storedAny = false;
          for (const candidate of candidates) {
            try {
              const userProfile = isUserProfileTaxonomy(candidate.taxonomy);
              const capture = await captureWithOutcomeReconciliation(native, {
                candidate: {
                  ...candidate,
                  source: `session:${event.properties.sessionID}:compaction${userProfile ? ":user" : ""}`,
                  scope: "project",
                  origin: "auto_compaction",
                  revive: false,
                },
                significance: candidate.importance,
                impact: candidate.kind === "decision" || candidate.kind === "gotcha" ? 0.8 : 0.6,
                rarity: candidate.code_paths.length > 0 ? 0.7 : 0.5,
                source_trust: userProfile ? "user" : "agent",
                has_valid_evidence: userProfile || candidate.code_paths.length > 0,
                suggested_supersession_ids: [],
                suggested_conflict_ids: [],
              });
              storedAny ||= capture.storedOrDuplicate;
            } catch (error) {
              session.warnOnce(error);
            }
          }
          if (storedAny) {
            session.invalidateRecall();
            maintenance.schedule();
          }
        } catch (error) {
          session.warnOnce(error);
        }
      },
      "chat.message": async (input, output) => {
        if (extractionSessions.has(input.sessionID)) return;
        if (input.model) {
          graphModels.set(input.sessionID, {
            providerID: input.model.providerID,
            modelID: input.model.modelID,
            ...(input.variant === undefined ? {} : { variant: input.variant }),
          });
        }
        if (input.agent) session.sessionAgents.set(input.sessionID, input.agent);
        const discoveryAgent =
          input.agent ?? session.sessionAgents.get(input.sessionID) ?? "unknown";
        void graphAuthorization(input.sessionID, discoveryAgent)
          .then(scheduleGraphDiscovery)
          .catch(session.warnOnce);
        session.latestQuery.delete(input.sessionID);
        session.invalidateRecall(input.sessionID);
        session.discardPendingRecall(input.sessionID);
        const query = deriveRecallQuery(output.parts);
        if (!query) return;
        session.latestQuery.set(input.sessionID, {
          query: truncateText(query, 2_000),
          agent: input.agent,
        });
      },
      "experimental.chat.system.transform": async (input, output) => {
        if (input.sessionID && extractionSessions.has(input.sessionID)) return;
        if (!output.system.some((entry) => entry.includes(MEMORY_INSTRUCTIONS_MARKER))) {
          output.system.push(memoryInstructions.content);
        }
        if (!input.sessionID) return;
        if (!settings.automaticRecall) return;
        const sessionID = input.sessionID;
        const latest = session.latestQuery.get(sessionID);
        if (!latest) return;
        try {
          await syncProjectIndexes();
        } catch (error) {
          session.warnOnce(error);
        }
        if (session.latestQuery.get(input.sessionID) !== latest) return;
        const rootSessionID = await session.resolveSessionRoot(input.sessionID);
        const agent = latest.agent ?? session.sessionAgents.get(input.sessionID) ?? "unknown";
        const budgetChars = contextBudgetChars(input.model);
        const recallGeneration = session.recallGeneration(input.sessionID);
        const cacheKey = [
          latest.query,
          rootSessionID,
          agent,
          budgetChars,
          sharedSignature ?? "none",
          recallGeneration,
        ].join("\0");

        let cached = session.recallCache.get(input.sessionID);
        if (!cached || cached.key !== cacheKey) {
          session.discardPendingRecall(input.sessionID);
          if (
            session.latestQuery.get(input.sessionID) !== latest ||
            session.recallGeneration(input.sessionID) !== recallGeneration
          ) {
            return;
          }
          try {
            const response = await session.searchRecallOnce(input.sessionID, cacheKey, async () => {
              const response = await native.request<SearchResponse>("search", {
                query: latest.query,
                max_results: 20,
                budget_chars: budgetChars,
                kinds: [],
                scopes: [],
                taxonomies: [],
                session_scope_key: rootSessionID,
                agent_scope_key: agent,
                min_score: settings.minScore,
                include_stale: false,
                include_superseded: false,
                include_graph: true,
                track_feedback: settings.feedbackTracking,
              });
              for (const warning of response.warnings) session.warnOnce(new Error(warning));
              return response;
            });
            if (
              session.latestQuery.get(input.sessionID) !== latest ||
              session.recallGeneration(input.sessionID) !== recallGeneration
            ) {
              return;
            }
            cached = { key: cacheKey, response };
            session.recallCache.set(input.sessionID, cached);
          } catch (error) {
            session.warnOnce(error);
            return;
          }
        }
        const formatted = formatRecalledMemories(cached.response, budgetChars);
        if (!formatted) return;
        if (!settings.feedbackTracking || !cached.response.retrieval_id) {
          output.system.push(formatted.text);
          return;
        }
        const pending = {
          retrievalID: cached.response.retrieval_id,
          memoryIDs: formatted.memoryIDs,
        };
        const opened = await session.openPendingRecall(sessionID, pending, () => {
          return (
            session.latestQuery.get(sessionID) === latest &&
            session.recallGeneration(sessionID) === recallGeneration
          );
        });
        if (opened) output.system.push(formatted.text);
      },
      "experimental.session.compacting": async (input, output) => {
        if (extractionSessions.has(input.sessionID)) return;
        output.context.push(COMPACTION_CONTEXT);
      },
      tool: {
        memory_model_profiles: tool({
          description:
            "List daemon-approved embedding model profiles for the current project. This is read-only and does not synchronize documents or load an embedding model.",
          args: {},
          async execute(_args, context) {
            const response = await listModelProfiles(native, context.abort);
            return result("Embedding model profiles", response, {
              active_profile_id: response.active_profile_id,
              active_generation_id: response.active_generation_id,
              profile_count: response.profiles.length,
              selectable_count: response.profiles.filter((profile) => profile.selectable).length,
            });
          },
        }),
        memory_model_switch: tool({
          description:
            "Start or preflight a durable project embedding-generation migration. Apply creates a new generation and returns immediately; it never changes environment variables or restarts the daemon.",
          args: {
            profile_id: tool.schema.string().min(1).max(128),
            allow_dense_downtime: tool.schema.boolean().default(false),
            force_rebuild: tool.schema.boolean().default(false),
            expected_active_profile_id: tool.schema.string().min(1).max(128).optional(),
            expected_active_generation_id: tool.schema.string().min(1).max(128).optional(),
            retain_previous: tool.schema.boolean().default(true),
            target_generation_id: tool.schema
              .string()
              .regex(/^(?:legacy|gen_[A-Za-z0-9_]{1,60})$/)
              .optional(),
            dry_run: tool.schema.boolean().default(false),
          },
          async execute(args, context) {
            if (!args.dry_run) {
              await context.ask({
                permission: "memory_model_switch",
                patterns: [args.profile_id],
                always: [],
                metadata: { operation: "model_switch", ...args },
              });
            }
            const switchID = `switch_${randomUUID().replaceAll("-", "")}`;
            let response: Awaited<ReturnType<typeof startModelSwitch>> | undefined;
            let reconciledStatus: Awaited<ReturnType<typeof getModelSwitchStatus>> | undefined;
            try {
              response = await startModelSwitch(
                native,
                {
                  ...args,
                  switch_id: switchID,
                },
                context.abort,
              );
            } catch (error) {
              if (!(error instanceof DaemonOutcomeUnknownError)) throw error;
              reconciledStatus = await getModelSwitchStatus(native, switchID, context.abort);
            }
            session.invalidateRecall();
            if (reconciledStatus) {
              if (!["succeeded", "cancelled", "failed"].includes(reconciledStatus.state)) {
                monitorModelSwitch(switchID);
              }
              return result(
                "Embedding model switch status after outcome-unknown",
                { ...reconciledStatus },
                { ...reconciledStatus },
              );
            }
            if (!response) throw new Error("Native memory omitted model switch response");
            if (!args.dry_run && response.switch_id) monitorModelSwitch(response.switch_id);
            return result(
              args.dry_run ? "Embedding model switch preflight" : "Started embedding model switch",
              response,
              {
                switch_id: response.switch_id,
                target_profile_id: response.target_profile_id,
                state: response.state,
                ...(args.dry_run
                  ? {
                      can_start: response.preflight.can_start,
                      blocker_count: response.preflight.blockers.length,
                    }
                  : {}),
              },
            );
          },
        }),
        memory_model_switch_status: tool({
          description:
            "Read durable embedding model-switch progress without syncing documents or loading a model.",
          args: {
            switch_id: tool.schema.string().regex(/^switch_[A-Za-z0-9_-]{1,120}$/),
          },
          async execute(args, context) {
            const response = await getModelSwitchStatus(native, args.switch_id, context.abort);
            if (["succeeded", "cancelled", "failed"].includes(response.state)) {
              session.invalidateRecall();
              flushDeferredProjectSync();
            }
            return result("Embedding model switch status", { ...response }, { ...response });
          },
        }),
        memory_model_switch_cancel: tool({
          description:
            "Request cooperative cancellation of a durable embedding model switch before cutover.",
          args: {
            switch_id: tool.schema.string().regex(/^switch_[A-Za-z0-9_-]{1,120}$/),
          },
          async execute(args, context) {
            await context.ask({
              permission: "memory_model_switch",
              patterns: [args.switch_id],
              always: [],
              metadata: { operation: "model_switch_cancel", ...args },
            });
            const response = await cancelModelSwitch(native, args.switch_id, context.abort);
            if (["already_committed", "already_terminal"].includes(response.outcome)) {
              session.invalidateRecall();
              flushDeferredProjectSync();
            }
            return result("Cancelled embedding model switch", { ...response }, { ...response });
          },
        }),
        memory_graph_extract: tool({
          description:
            "Explicitly extract source-backed knowledge graph candidates with the current OpenCode provider, then validate and persist them in the native project graph. This sends eligible source units to the provider and always asks permission first.",
          args: {
            source_memory_ids: tool.schema
              .array(tool.schema.string().min(1).max(128))
              .min(1)
              .max(64),
            dry_run: tool.schema
              .boolean()
              .default(false)
              .describe("Return validated candidates without persisting graph facts."),
            retry_count: tool.schema.number().int().min(0).max(3).default(2),
          },
          async execute(args, context) {
            const model = graphModels.get(context.sessionID);
            if (!model) {
              throw new Error(
                "The active OpenCode provider/model is unknown. Send a normal chat message in this session before graph extraction.",
              );
            }
            const authorization = await graphAuthorization(context.sessionID, context.agent);
            const prepared = await native.request<GraphPrepareResponse>(
              "graph_extract_prepare",
              {
                authorization,
                source_memory_ids: args.source_memory_ids,
                max_units: 64,
                max_unit_text_bytes: 32 * 1_024,
                max_total_text_bytes: 256 * 1_024,
              },
              context.abort,
            );
            const eligible = prepared.units.filter(
              (unit): unit is GraphPreparedUnit & { source: GraphSourceBinding } =>
                unit.source?.remote_eligible === true,
            );
            if (eligible.length === 0) {
              return result(
                "Knowledge graph extraction blocked",
                {
                  persisted: false,
                  rejected_sources: prepared.rejected_sources,
                  warnings: prepared.warnings,
                  message: "No requested source is eligible for remote extraction.",
                },
                {
                  eligible_source_count: 0,
                  rejected_source_count: prepared.rejected_sources.length,
                },
              );
            }
            const totalBytes = eligible.reduce(
              (total, unit) => total + new TextEncoder().encode(unit.text).byteLength,
              0,
            );
            await context.ask({
              permission: "memory_graph_extract",
              patterns: [`${model.providerID}/${model.modelID}`],
              always: [],
              metadata: {
                provider_id: model.providerID,
                model_id: model.modelID,
                source_count: eligible.length,
                source_bytes: totalBytes,
              },
            });
            if (args.dry_run) {
              const extractor = createGraphExtractor(
                { serverUrl, directory },
                {
                  ...model,
                  retryCount: args.retry_count,
                  onSessionChange: (sessionID, active) => {
                    if (active) extractionSessions.add(sessionID);
                    else extractionSessions.delete(sessionID);
                  },
                },
              );
              const candidates = await extractor.extract(
                eligible.map((unit) => ({
                  source_unit_id: unit.source.source_unit_id,
                  text: unit.text,
                })),
                context.abort,
              );
              return result(
                "Knowledge graph extraction dry run",
                {
                  persisted: false,
                  provider: model,
                  candidates,
                  rejected_sources: prepared.rejected_sources,
                  warnings: prepared.warnings,
                },
                {
                  source_count: eligible.length,
                  entity_count: candidates.entities.length,
                  relation_count: candidates.relations.length,
                },
              );
            }
            const provider = {
              provider_id: model.providerID,
              model_id: model.modelID,
              extractor_version: GRAPH_EXTRACTOR_VERSION,
              prompt_version: GRAPH_PROMPT_VERSION,
              schema_version: GRAPH_CANDIDATE_SCHEMA_VERSION,
              ...(model.variant === undefined ? {} : { variant: model.variant }),
            } satisfies GraphProviderIdentity;
            const maxAttempts = args.retry_count + 1;
            const jobID = graphExtractionJobID(
              eligible.map((unit) => unit.source),
              provider,
              maxAttempts,
            );
            let response: GraphExtractEnqueueResponse;
            try {
              response = await native.request<GraphExtractEnqueueResponse>(
                "graph_extract_enqueue",
                {
                  authorization,
                  job_id: jobID,
                  source_memory_ids: eligible.map((unit) => unit.source.source_memory_id),
                  provider,
                  max_attempts: maxAttempts,
                  max_unit_text_bytes: 32 * 1_024,
                  max_total_text_bytes: 256 * 1_024,
                },
                context.abort,
              );
            } catch (error) {
              scheduleGraphDiscovery(authorization);
              throw error;
            }
            const job = response.job;
            if (!job) throw new Error("Native memory daemon omitted the enqueued graph job");
            scheduleGraphJob(job.job_id, authorization);
            return result(
              response.existing
                ? "Knowledge graph extraction already queued"
                : "Queued knowledge graph extraction",
              response,
              {
                job_id: job.job_id,
                state: job.state,
                source_count: job.sources.length,
                attempt_count: job.attempt_count,
                max_attempts: job.max_attempts,
              },
            );
          },
        }),
        memory_graph_extract_status: tool({
          description:
            "Inspect a durable graph extraction job. Queued or lease-recovering work is resumed by this plugin worker without creating a duplicate job.",
          args: {
            job_id: tool.schema.string().min(1).max(128),
          },
          async execute(args, context) {
            const authorization = await graphAuthorization(context.sessionID, context.agent);
            const response = await native.request<GraphExtractJobStatusResponse>(
              "graph_extract_job_status",
              { authorization, job_id: args.job_id },
              context.abort,
            );
            if (
              response.found &&
              response.job &&
              !["completed", "failed", "cancelled"].includes(response.job.state)
            ) {
              scheduleGraphJob(response.job.job_id, authorization);
            }
            return result("Knowledge graph extraction job", response, {
              job_id: args.job_id,
              found: response.found,
              state: response.job?.state ?? "not_found",
            });
          },
        }),
        memory_graph_extract_cancel: tool({
          description:
            "Cancel a durable graph extraction job. Active provider work stops cooperatively.",
          args: {
            job_id: tool.schema.string().min(1).max(128),
            reason: tool.schema.string().max(512).default("cancelled by user"),
          },
          async execute(args, context) {
            const authorization = await graphAuthorization(context.sessionID, context.agent);
            const cancelRequest = {
              authorization,
              job_id: args.job_id,
              reason: boundedGraphJobError(args.reason),
            };
            let response: GraphExtractCancelResponse;
            try {
              response = await native.request<GraphExtractCancelResponse>(
                "graph_extract_cancel",
                cancelRequest,
                context.abort,
              );
            } catch (error) {
              if (!(error instanceof DaemonOutcomeUnknownError)) throw error;
              const status = await graphJobStatus(args.job_id, authorization, context.abort);
              if (!status.found || !status.job || status.job.state === "queued") {
                response = await native.request<GraphExtractCancelResponse>(
                  "graph_extract_cancel",
                  cancelRequest,
                  context.abort,
                );
              } else {
                response = {
                  job: status.job,
                  outcome: status.job.cancel_requested
                    ? "cancel_requested"
                    : status.job.state === "cancelled"
                      ? "cancelled"
                      : "already_terminal",
                };
              }
            }
            if (response.outcome === "cancel_requested" || response.outcome === "cancelled") {
              graphAttemptAborts
                .get(args.job_id)
                ?.abort(new Error("Graph extraction was cancelled"));
            }
            return result("Cancelled knowledge graph extraction", response, {
              job_id: args.job_id,
              outcome: response.outcome,
              state: response.job?.state ?? "not_found",
            });
          },
        }),
        memory_graph_search: tool({
          description:
            "Search source-backed entities and relations in the native project knowledge graph with bounded traversal.",
          args: {
            query: tool.schema.string().min(1).max(2_000),
            limit: tool.schema.number().int().min(1).max(64).default(20),
            max_depth: tool.schema.number().int().min(0).max(2).default(2),
            max_fanout: tool.schema.number().int().min(1).max(32).default(32),
            as_of_ms: tool.schema
              .number()
              .int()
              .min(0)
              .max(Number.MAX_SAFE_INTEGER)
              .optional()
              .describe(
                "Optional historical instant. Relation validity starts inclusively and ends exclusively.",
              ),
          },
          async execute(args, context) {
            const authorization = await graphAuthorization(context.sessionID, context.agent);
            const request = {
              authorization,
              query: args.query,
              ...(args.as_of_ms === undefined
                ? {}
                : {
                    time: {
                      valid_after_ms: args.as_of_ms,
                      valid_before_ms: args.as_of_ms,
                    },
                  }),
              max_depth: args.max_depth,
              max_fanout: args.max_fanout,
              max_results: args.limit,
              max_evidence_per_fact: 8,
            } satisfies GraphSearchRequest;
            const response = await native.request<GraphSearchResponse>(
              "graph_search",
              request,
              context.abort,
            );
            return result("Knowledge graph search", response, {
              query: args.query,
              ...(args.as_of_ms === undefined ? {} : { as_of_ms: args.as_of_ms }),
            });
          },
        }),
        memory_graph_status: tool({
          description: "Inspect source-visible native knowledge graph counts and last extraction.",
          args: {},
          async execute(_args, context) {
            const authorization = await graphAuthorization(context.sessionID, context.agent);
            const request = { authorization } satisfies GraphStatusRequest;
            const response = await native.request<GraphStatusResponse>(
              "graph_status",
              request,
              context.abort,
            );
            return result("Knowledge graph status", response, {});
          },
        }),
        memory_graph_export: tool({
          description:
            "Export one bounded page of source-visible entities, relations, and provenance from the native knowledge graph.",
          args: {
            cursor: tool.schema.string().max(32).optional(),
            limit: tool.schema.number().int().min(1).max(100).default(50),
          },
          async execute(args, context) {
            const authorization = await graphAuthorization(context.sessionID, context.agent);
            const request = {
              authorization,
              ...(args.cursor === undefined ? {} : { cursor: args.cursor }),
              page_limit: args.limit,
            } satisfies GraphExportRequest;
            const response = await native.request<GraphExportResponse>(
              "graph_export",
              request,
              context.abort,
            );
            return result("Knowledge graph export", response, { cursor: args.cursor ?? "0" });
          },
        }),
        memory_search: tool({
          description:
            "Semantically search durable memory for the current project. Use before substantial work when prior decisions, preferences, facts, patterns, or gotchas may matter.",
          args: {
            query: tool.schema
              .string()
              .min(1)
              .max(2_000)
              .describe("Concise task-specific retrieval query."),
            retrieval_mode: tool.schema
              .enum(RETRIEVAL_MODES)
              .default("hybrid")
              .describe("Retrieval channel used for search and benchmark comparisons."),
            limit: tool.schema
              .number()
              .int()
              .min(1)
              .max(20)
              .default(20)
              .describe("Safety ceiling; context budget normally decides the count."),
            budget_chars: tool.schema
              .number()
              .int()
              .min(512)
              .max(24_000)
              .default(6_000)
              .describe("Maximum serialized memory context in characters."),
            kinds: tool.schema
              .array(tool.schema.enum(MEMORY_KINDS))
              .max(MEMORY_KINDS.length)
              .default([])
              .describe("Optional memory kinds to include."),
            scopes: tool.schema
              .array(tool.schema.enum(MEMORY_SCOPES))
              .max(MEMORY_SCOPES.length)
              .default([])
              .describe("Optional scopes to include."),
            taxonomies: tool.schema
              .array(tool.schema.enum(MEMORY_TAXONOMIES))
              .max(MEMORY_TAXONOMIES.length)
              .default([])
              .describe("Optional CoALA-derived taxonomies to include."),
            min_score: tool.schema
              .number()
              .min(0)
              .max(1)
              .default(settings.minScore)
              .describe("Minimum calibrated relevance score."),
            include_stale: tool.schema
              .boolean()
              .default(false)
              .describe("Include memories whose code anchors changed."),
            include_superseded: tool.schema
              .boolean()
              .default(false)
              .describe("Include historical memories replaced by a successor."),
            include_graph: tool.schema
              .boolean()
              .default(false)
              .describe("Fuse source-backed knowledge-graph memory IDs with normal retrieval."),
          },
          async execute(args, context) {
            session.discardPendingRecall(context.sessionID);
            await syncProjectIndexes();
            const rootSessionID = await session.resolveSessionRoot(context.sessionID);
            const response = await native.request<SearchResponse>(
              "search",
              {
                query: args.query,
                retrieval_mode: args.retrieval_mode,
                max_results: args.limit,
                budget_chars: args.budget_chars,
                kinds: args.kinds,
                scopes: args.scopes,
                taxonomies: args.taxonomies,
                session_scope_key: rootSessionID,
                agent_scope_key: context.agent,
                min_score: args.min_score,
                include_stale: args.include_stale,
                include_superseded: args.include_superseded,
                include_graph: args.include_graph,
                track_feedback: settings.feedbackTracking,
              },
              context.abort,
            );
            for (const warning of response.warnings) session.warnOnce(new Error(warning));
            if (
              settings.feedbackTracking &&
              response.retrieval_id &&
              response.memories.length > 0
            ) {
              const pending = {
                retrievalID: response.retrieval_id,
                memoryIDs: response.memories.map((memory) => memory.id),
              };
              await session.openPendingRecall(context.sessionID, pending);
            }
            return result("Memory search", response, {
              count: response.count,
              retrieval_id: response.retrieval_id,
              abstained: response.abstained,
            });
          },
        }),
        memory_store: tool({
          description:
            "Store one distilled, durable project memory or explicit default_user personalization observation. Never store secrets, raw conversations, inferred sensitive traits, temporary logs, unverified guesses, or guessed code_paths.",
          args: {
            content: tool.schema
              .string()
              .min(1)
              .max(6_000)
              .describe("Self-contained durable fact or concise summary."),
            title: tool.schema
              .string()
              .min(1)
              .max(160)
              .optional()
              .describe("Short descriptive title; inferred when omitted."),
            kind: tool.schema
              .enum(MEMORY_KINDS)
              .default("fact")
              .describe("Durable memory category."),
            importance: tool.schema
              .number()
              .min(0)
              .max(1)
              .default(0.7)
              .describe("Long-term importance from 0 to 1."),
            tags: tool.schema
              .array(tool.schema.string().min(1).max(64))
              .max(12)
              .default([])
              .describe("Short retrieval tags."),
            scope: tool.schema
              .enum(WRITABLE_MEMORY_SCOPES)
              .default("project")
              .describe(
                "session shares with the parent/subagent family; agent is role-specific; project is durable and private.",
              ),
            expires_in_days: tool.schema
              .number()
              .int()
              .min(1)
              .max(3_650)
              .optional()
              .describe("Optional hard expiry override."),
            code_paths: tool.schema
              .array(tool.schema.string().min(1).max(512))
              .max(12)
              .default([])
              .describe(
                "Existing regular files relative to the project root that verify this memory. Never guess paths; leave empty when no verified file applies. Any invalid path rejects the store.",
              ),
            revive: tool.schema
              .boolean()
              .default(false)
              .describe("Revive a tombstoned memory after user approval."),
            taxonomy: tool.schema
              .enum(MEMORY_TAXONOMIES)
              .optional()
              .describe(
                "Explicit taxonomy. Use user_identity, user_behavior, user_preference, user_goal, or user_relationship only for information stated directly by default_user.",
              ),
            evidence_quote: tool.schema
              .string()
              .min(3)
              .max(500)
              .optional()
              .describe(
                "Required for user_* personalization taxonomies: a short verbatim excerpt from the current default_user message. Used only for provenance validation and never stored.",
              ),
            confidence: tool.schema
              .number()
              .min(0)
              .max(1)
              .optional()
              .describe("Confidence in this memory; defaults from importance."),
          },
          async execute(args, context) {
            const { evidence_quote: evidenceQuote, ...storeArgs } = args;
            const userProfile = isUserProfileTaxonomy(storeArgs.taxonomy);
            if (userProfile) {
              const expectedKind = storeArgs.taxonomy === "user_preference" ? "preference" : "fact";
              if (storeArgs.kind !== expectedKind) {
                throw new Error(`${storeArgs.taxonomy} requires memory kind ${expectedKind}`);
              }
              const currentUserText = session.latestQuery.get(context.sessionID)?.query;
              if (!currentUserText || !hasDirectUserEvidence(evidenceQuote, [currentUserText])) {
                throw new Error(
                  `${storeArgs.taxonomy} requires evidence_quote copied verbatim from the current default_user message`,
                );
              }
            }
            if (args.revive) {
              await context.ask({
                permission: "memory_revive",
                patterns: [args.title ?? truncateText(args.content, 80)],
                always: [],
                metadata: { operation: "revive", scope: args.scope },
              });
            }
            const key = await session.scopeKey(args.scope, context.sessionID, context.agent);
            const response = await native.request<Record<string, unknown>>(
              "store",
              {
                ...storeArgs,
                scope_key: key,
                origin: "manual",
                source: `session:${context.sessionID}`,
              },
              context.abort,
            );
            session.invalidateRecall();
            maintenance.schedule();
            return result("Stored memory", response, {
              id: response.id,
              inserted: response.inserted,
            });
          },
        }),
        memory_ingest: tool({
          description:
            "Queue a project-local PDF, Markdown, or HTML document for background ingestion. Returns immediately with a job_id; poll memory_ingest_status for the result.",
          args: {
            path: tool.schema
              .string()
              .min(1)
              .max(200)
              .describe("Existing document path relative to the project root."),
            title: tool.schema
              .string()
              .min(1)
              .max(120)
              .optional()
              .describe("Optional document title; defaults to the filename."),
            kind: tool.schema.enum(MEMORY_KINDS).default("fact"),
            importance: tool.schema.number().min(0).max(1).default(0.6),
            tags: tool.schema.array(tool.schema.string().min(1).max(64)).max(11).default([]),
            scope: tool.schema
              .enum(WRITABLE_MEMORY_SCOPES)
              .default("project")
              .describe(
                "session shares within the parent/subagent family; agent is role-specific; project is durable and private.",
              ),
            expires_in_days: tool.schema.number().int().min(1).max(3_650).optional(),
            revive: tool.schema
              .boolean()
              .default(false)
              .describe("Revive matching tombstoned chunks after user approval."),
            taxonomy: tool.schema.enum(MEMORY_TAXONOMIES).optional(),
            confidence: tool.schema.number().min(0).max(1).optional(),
          },
          async execute(args, context) {
            await context.ask({
              permission: "memory_ingest",
              patterns: [args.path],
              always: [],
              metadata: { operation: "ingest", scope: args.scope },
            });
            if (args.revive) {
              await context.ask({
                permission: "memory_revive",
                patterns: [args.path],
                always: [],
                metadata: { operation: "revive", scope: args.scope },
              });
            }
            const scopeKey = await session.scopeKey(args.scope, context.sessionID, context.agent);
            if (context.abort.aborted) throw new Error("Background memory ingestion was cancelled");
            const request = { ...args, scope_key: scopeKey };
            const job = ingestJobs.enqueue(
              { path: args.path, scope: args.scope },
              async (signal) => {
                try {
                  const response = await native.request<IngestResponse>("ingest", request, signal);
                  if (signal.aborted) throw new Error("Background memory ingestion was cancelled");
                  for (const warning of response.warnings) session.warnOnce(new Error(warning));
                  session.invalidateRecall();
                  maintenance.schedule();
                  return response;
                } catch (error) {
                  if (!signal.aborted) session.warnOnce(error);
                  throw error;
                }
              },
            );
            const currentJob = ingestJobs.get(job.job_id) ?? job;
            return result("Started document ingestion", currentJob, {
              job_id: currentJob.job_id,
              status: currentJob.status,
              path: args.path,
            });
          },
        }),
        memory_ingest_status: tool({
          description:
            "Poll background memory_ingest jobs. Returns queued, running, succeeded with the ingestion result, or failed with an error message.",
          args: {
            job_ids: tool.schema
              .array(tool.schema.string().regex(BACKGROUND_JOB_ID_PATTERN))
              .min(1)
              .max(100)
              .optional()
              .describe("Job IDs returned by memory_ingest. Omit to list recent jobs."),
          },
          async execute(args) {
            const jobs = ingestJobs.list(args.job_ids);
            return result(
              "Document ingestion jobs",
              { jobs, count: jobs.length },
              {
                count: jobs.length,
              },
            );
          },
        }),
        memory_index_documents: tool({
          description:
            "Incrementally discover and index supported project documents while respecting .gitignore and .ignore files. Unchanged files are skipped and deleted files are removed from the derived index.",
          args: {
            force: tool.schema
              .boolean()
              .default(false)
              .describe(
                "Re-extract every discovered document even when its content hash is unchanged.",
              ),
          },
          async execute(args, context) {
            await context.ask({
              permission: "memory_ingest",
              patterns: ["**/*.{pdf,md,markdown,html,htm}"],
              always: [],
              metadata: { operation: "index_documents", force: args.force },
            });
            const response = await indexDocuments(args.force);
            session.invalidateRecall();
            return result("Indexed project documents", response, {
              discovered: response.discovered,
              added: response.added,
              updated: response.updated,
              removed: response.removed,
              rejected: response.rejected,
            });
          },
        }),
        memory_get: tool({
          description: "Fetch complete durable memories by IDs returned from memory_search.",
          args: {
            ids: tool.schema
              .array(tool.schema.string().regex(/^mem_[0-9a-f]{32}$/))
              .min(1)
              .max(100)
              .describe("Memory IDs to fetch."),
          },
          async execute(args, context) {
            const keys = await session.managementScopeKeys(context.sessionID, context.agent);
            const response = await native.request<MemoryRecord[]>(
              "get",
              { ...args, ...keys },
              context.abort,
            );
            return result("Memories", response, {
              count: response.length,
            });
          },
        }),
        memory_list: tool({
          description:
            "List lifecycle-indexed memories for review, cleanup, and /memory management.",
          args: {
            kinds: tool.schema
              .array(tool.schema.enum(MEMORY_KINDS))
              .max(MEMORY_KINDS.length)
              .default([]),
            scopes: tool.schema
              .array(tool.schema.enum(MEMORY_SCOPES))
              .max(MEMORY_SCOPES.length)
              .default([]),
            taxonomies: tool.schema
              .array(tool.schema.enum(MEMORY_TAXONOMIES))
              .max(MEMORY_TAXONOMIES.length)
              .default([]),
            include_expired: tool.schema.boolean().default(false),
            include_stale: tool.schema.boolean().default(false),
            include_superseded: tool.schema.boolean().default(false),
            offset: tool.schema.number().int().min(0).default(0),
            limit: tool.schema.number().int().min(1).max(100).default(50),
          },
          async execute(args, context) {
            await syncProjectIndexes();
            const keys = await session.managementScopeKeys(context.sessionID, context.agent);
            const response = await native.request<ListResponse>(
              "list",
              { ...args, ...keys },
              context.abort,
            );
            return result("Memory list", response, {
              total: response.total,
              count: response.count,
            });
          },
        }),
        memory_update: tool({
          description:
            "Correct or reclassify one local memory by ID with optional optimistic concurrency.",
          args: {
            id: tool.schema.string().regex(/^mem_[0-9a-f]{32}$/),
            expected_updated_at_ms: tool.schema.number().int().optional(),
            content: tool.schema.string().min(1).max(6_000).optional(),
            title: tool.schema.string().min(1).max(160).optional(),
            kind: tool.schema.enum(MEMORY_KINDS).optional(),
            importance: tool.schema.number().min(0).max(1).optional(),
            tags: tool.schema.array(tool.schema.string().min(1).max(64)).max(12).optional(),
            scope: tool.schema.enum(WRITABLE_MEMORY_SCOPES).optional(),
            expires_in_days: tool.schema.number().int().min(1).max(3_650).optional(),
            clear_expiry: tool.schema.boolean().default(false),
            code_paths: tool.schema
              .array(tool.schema.string().min(1).max(512))
              .max(12)
              .optional()
              .describe(
                "Replacement anchors: verified existing regular files relative to the project root. Omit to preserve current anchors; pass [] to clear them. Any invalid path rejects the update.",
              ),
            pinned: tool.schema
              .boolean()
              .optional()
              .describe("Pin the memory so it bypasses expiry and retention decay."),
            lock_action: tool.schema
              .enum(LOCK_ACTIONS)
              .optional()
              .describe("Lock or unlock the memory. Locked records block updates and deletes."),
            lock_reason: tool.schema
              .string()
              .min(1)
              .max(240)
              .optional()
              .describe("Reason for locking the memory. Only valid with lock_action='lock'."),
            taxonomy: tool.schema
              .enum(MEMORY_TAXONOMIES)
              .optional()
              .describe("Explicit reclassification; omit to preserve the current taxonomy."),
            confidence: tool.schema.number().min(0).max(1).optional(),
            conflict_with: tool.schema
              .array(tool.schema.string().regex(/^mem_[0-9a-f]{32}$/))
              .max(100)
              .optional()
              .describe("Symmetric conflict links; pass [] to clear links."),
          },
          async execute(args, context) {
            validateUpdateArgs(args);
            const keys = await session.managementScopeKeys(context.sessionID, context.agent);
            const existing = await native.request<MemoryRecord[]>(
              "get",
              { ids: [args.id], ...keys },
              context.abort,
            );
            const record = existing[0];
            if (!record) throw new Error(`Memory not found: ${args.id}`);
            if (record.scope === "repository") {
              throw new Error(
                "Repository memory is canonical Markdown; edit its .opencode/memory file instead.",
              );
            }
            const key = args.scope
              ? await session.scopeKey(args.scope, context.sessionID, context.agent)
              : undefined;
            const response = await native.request<Record<string, unknown>>(
              "update",
              { ...args, scope_key: key, ...keys },
              context.abort,
            );
            session.invalidateRecall();
            maintenance.schedule();
            return result("Updated memory", response, response);
          },
        }),
        memory_pin: tool({
          description:
            "Pin or unpin one local memory without re-embedding or refreshing semantic recency.",
          args: {
            id: tool.schema.string().regex(/^mem_[0-9a-f]{32}$/),
            pinned: tool.schema.boolean(),
            expected_updated_at_ms: tool.schema.number().int().optional(),
          },
          async execute(args, context) {
            const keys = await session.managementScopeKeys(context.sessionID, context.agent);
            const response = await native.request<Record<string, unknown>>(
              "pin",
              { ...args, ...keys },
              context.abort,
            );
            session.invalidateRecall();
            return result(args.pinned ? "Pinned memory" : "Unpinned memory", response, response);
          },
        }),
        memory_lock: tool({
          description:
            "Lock or unlock one local memory. Unlock is lifecycle-only; locked records reject semantic changes and deletion.",
          args: {
            id: tool.schema.string().regex(/^mem_[0-9a-f]{32}$/),
            lock_action: tool.schema.enum(LOCK_ACTIONS),
            lock_reason: tool.schema.string().min(1).max(240).optional(),
            expected_updated_at_ms: tool.schema.number().int().optional(),
          },
          async execute(args, context) {
            if (args.lock_action === "unlock" && args.lock_reason !== undefined) {
              throw new Error("lock_reason is valid only when locking");
            }
            const keys = await session.managementScopeKeys(context.sessionID, context.agent);
            const response = await native.request<Record<string, unknown>>(
              "lock",
              { ...args, ...keys },
              context.abort,
            );
            session.invalidateRecall();
            return result(
              args.lock_action === "lock" ? "Locked memory" : "Unlocked memory",
              response,
              response,
            );
          },
        }),
        memory_delete: tool({
          description:
            "Batch-delete obsolete or incorrect memories and leave tombstones by default.",
          args: {
            ids: tool.schema
              .array(tool.schema.string().regex(/^mem_[0-9a-f]{32}$/))
              .min(1)
              .max(100),
            tombstone: tool.schema.boolean().default(true),
            reason: tool.schema
              .enum(["obsolete", "incorrect", "user_deleted"])
              .default("user_deleted"),
          },
          async execute(args, context) {
            const keys = await session.managementScopeKeys(context.sessionID, context.agent);
            const records = await native.request<MemoryRecord[]>(
              "get",
              { ids: args.ids, ...keys },
              context.abort,
            );
            validateDeleteRecords(records);
            await context.ask({
              permission: "memory_delete",
              patterns: args.ids,
              always: [],
              metadata: { operation: "delete", ...args },
            });
            const response = await native.request<Record<string, unknown>>(
              "delete",
              { ...args, ...keys },
              context.abort,
            );
            session.invalidateRecall();
            maintenance.schedule();
            return result("Deleted memories", response, response);
          },
        }),
        memory_feedback: tool({
          description:
            "Record whether recalled memory was used, ignored, or caused an error. Provide at least one exact recalled memory ID; skip this tool when no memory qualifies.",
          args: {
            retrieval_id: tool.schema
              .string()
              .regex(/^ret_[0-9a-f]{24}$/)
              .optional()
              .describe("Defaults to the latest pending retrieval in this session."),
            event: tool.schema.enum(FEEDBACK_EVENTS),
            memory_ids: tool.schema
              .array(tool.schema.string().regex(/^mem_[0-9a-f]{32}$/))
              .min(1)
              .max(100)
              .describe("Exact recalled memory IDs affected by this feedback."),
          },
          async execute(args, context) {
            const pending = session.pendingRecall.get(context.sessionID);
            const retrievalID = args.retrieval_id ?? pending?.retrievalID;
            if (!retrievalID) {
              throw new Error("No pending retrieval is available for this session");
            }
            const response = await native.request<Record<string, unknown>>(
              "feedback",
              {
                retrieval_id: retrievalID,
                event: args.event,
                memory_ids: args.memory_ids,
              },
              context.abort,
            );
            if (pending?.retrievalID === retrievalID) {
              session.pendingRecall.delete(context.sessionID);
            }
            return result("Recorded memory feedback", response, response);
          },
        }),
        memory_promote: tool({
          description:
            "Promote one reviewed local memory to Git-shareable .opencode/memory Markdown.",
          args: {
            id: tool.schema.string().regex(/^mem_[0-9a-f]{32}$/),
          },
          async execute(args, context) {
            const keys = await session.managementScopeKeys(context.sessionID, context.agent);
            const memories = await native.request<MemoryRecord[]>(
              "get",
              { ids: [args.id], ...keys },
              context.abort,
            );
            const memory = memories[0];
            if (!memory) throw new Error(`Memory not found: ${args.id}`);
            if (memory.scope === "repository") {
              return result(
                "Memory already shared",
                { id: memory.id, source: memory.source },
                { id: memory.id },
              );
            }
            const destination = `${SHARED_MEMORY_RELATIVE_DIR}/${memory.id}.md`;
            await context.ask({
              permission: "memory_promote",
              patterns: [destination],
              always: [],
              metadata: {
                operation: "promote",
                id: memory.id,
                title: memory.title,
                destination,
              },
            });
            const path = await writeSharedMemory(memoryProjectRoot, memory);
            await syncSharedMemories(true);
            return result("Promoted memory", { id: memory.id, path }, { id: memory.id, path });
          },
        }),
        memory_export: tool({
          description:
            "Export visible memories, lifecycle relations, and tombstones as a portable JSON snapshot.",
          args: {
            include_expired: tool.schema.boolean().default(true),
            include_superseded: tool.schema.boolean().default(true),
          },
          async execute(args, context) {
            const keys = await session.managementScopeKeys(context.sessionID, context.agent);
            const snapshot = await native.request<Record<string, unknown>>(
              "export",
              { ...args, ...keys },
              context.abort,
            );
            return result("Memory snapshot", snapshot, {
              format_version: snapshot.format_version,
              source_project_id: snapshot.source_project_id,
            });
          },
        }),
        memory_import: tool({
          description:
            "Import a native-memory JSON snapshot after validating IDs, relations, lifecycle metadata, and content safety.",
          args: {
            snapshot_json: tool.schema
              .string()
              .min(2)
              .max(4_000_000)
              .describe("Exact JSON returned by memory_export."),
          },
          async execute(args, context) {
            let snapshot: unknown;
            try {
              snapshot = JSON.parse(args.snapshot_json);
            } catch (error) {
              throw new Error("snapshot_json is not valid JSON", { cause: error });
            }
            if (typeof snapshot !== "object" || snapshot === null || Array.isArray(snapshot)) {
              throw new Error("snapshot_json must contain a snapshot object");
            }
            await context.ask({
              permission: "memory_import",
              patterns: ["native-memory-snapshot"],
              always: [],
              metadata: { operation: "import" },
            });
            const response = await native.request<Record<string, unknown>>(
              "import",
              { snapshot },
              context.abort,
            );
            session.invalidateRecall();
            maintenance.schedule();
            return result("Imported memory snapshot", response, response);
          },
        }),
        memory_purge: tool({
          description:
            "Delete all local indexed memories for the current project. Shared Markdown files are preserved.",
          args: {
            project_id: tool.schema
              .string()
              .regex(/^[0-9a-f]{64}$/)
              .describe("Exact project ID from memory_status."),
            keep_tombstones: tool.schema.boolean().default(true),
          },
          async execute(args, context) {
            await context.ask({
              permission: "memory_purge",
              patterns: [args.project_id],
              always: [],
              metadata: { operation: "purge", ...args },
            });
            const response = await native.request<Record<string, unknown>>(
              "purge",
              args,
              context.abort,
            );
            session.invalidateRecall();
            session.pendingRecall.clear();
            sharedSignature = undefined;
            return result("Purged memory", response, response);
          },
        }),
        memory_optimize: tool({
          description:
            "Prune expired memories and retrieval logs, compact zvec, and rebuild indexes.",
          args: {},
          async execute(_args, context) {
            const { response } = await requestIdempotently<Record<string, unknown>>(
              native,
              "optimize",
              {},
              context.abort,
            );
            session.invalidateRecall();
            return result("Optimized memory", response, response);
          },
        }),
        memory_doctor: tool({
          description:
            "Diagnose state compatibility, index health, retention, code anchors, and model cache.",
          args: {
            deep: tool.schema
              .boolean()
              .default(false)
              .describe("Hash all code anchors to detect staleness."),
          },
          async execute(args, context) {
            await syncProjectIndexes();
            const response = await native.request<Record<string, unknown>>(
              "doctor",
              args,
              context.abort,
            );
            return result("Memory doctor", response, response);
          },
        }),
        memory_status: tool({
          description:
            "Health-check the memory plugin and inspect its native backend, collection, embedding model, indexes, and document count.",
          args: {},
          async execute(_args, context) {
            const backend = await Promise.allSettled([
              native.request<NativeMemoryStatus>("status", {}, context.abort),
            ]).then(([result]) => result);
            if (context.abort.aborted) {
              const reason = backend.status === "rejected" ? backend.reason : context.abort.reason;
              throw reason instanceof Error
                ? reason
                : new Error("Memory status check was cancelled");
            }
            if (backend.status === "fulfilled") maintenance.observeStatus(backend.value);
            const switching =
              backend.status === "fulfilled" &&
              backend.value.switch_state !== null &&
              !["succeeded", "cancelled", "failed"].includes(backend.value.switch_state);
            if (switching) {
              deferredProjectSync = true;
              scheduleDeferredSwitchMonitor();
            }
            let sharedSyncResult: PromiseSettledResult<void>;
            let documentIndexResult: PromiseSettledResult<DocumentIndexResponse | undefined>;
            if (switching) {
              sharedSyncResult = { status: "fulfilled", value: undefined };
              documentIndexResult = { status: "fulfilled", value: undefined };
            } else {
              [sharedSyncResult, documentIndexResult] = await Promise.all([
                Promise.allSettled([syncSharedMemories()]).then(([result]) => result),
                Promise.allSettled([
                  settings.automaticDocumentIndex
                    ? indexDocuments()
                    : Promise.resolve<DocumentIndexResponse | undefined>(undefined),
                ]).then(([result]) => result),
              ]);
            }
            const response = buildMemoryStatusResponse(
              backend,
              sharedSyncResult,
              documentIndexResult,
            );
            return result("Memory status", response, response);
          },
        }),
      },
    };
  };
}

interface ResolvedMemoryPluginOptions {
  warmup: boolean;
  automaticRecall: boolean;
  automaticCapture: boolean;
  automaticDocumentIndex: boolean;
  documentIndexDebounceMs: number;
  automaticOptimize: boolean;
  optimizeDebounceMs: number;
  sharedSync: boolean;
  feedbackTracking: boolean;
  minScore: number;
}

export function resolveMemoryPluginOptions(
  options: MemoryPluginOptions,
): ResolvedMemoryPluginOptions {
  const minScore = options.minScore ?? envNumber("OPENCODE_MEMORY_MIN_SCORE", 0.42);
  if (!Number.isFinite(minScore) || minScore < 0 || minScore > 1) {
    throw new Error("memory minScore must be between 0 and 1");
  }
  const documentIndexDebounceMs =
    options.documentIndexDebounceMs ?? envNumber("OPENCODE_MEMORY_DOCUMENT_INDEX_DEBOUNCE_MS", 750);
  if (
    !Number.isFinite(documentIndexDebounceMs) ||
    documentIndexDebounceMs < 50 ||
    documentIndexDebounceMs > 60_000
  ) {
    throw new Error("memory documentIndexDebounceMs must be between 50 and 60000");
  }
  const optimizeDebounceMs =
    options.optimizeDebounceMs ??
    envNumber("OPENCODE_MEMORY_OPTIMIZE_DEBOUNCE_MS", DEFAULT_OPTIMIZE_DEBOUNCE_MS);
  if (
    !Number.isFinite(optimizeDebounceMs) ||
    optimizeDebounceMs < 50 ||
    optimizeDebounceMs > 60_000
  ) {
    throw new Error("memory optimizeDebounceMs must be between 50 and 60000");
  }
  return {
    warmup: options.warmup ?? envBoolean("OPENCODE_MEMORY_WARMUP", true),
    automaticRecall: options.automaticRecall ?? envBoolean("OPENCODE_MEMORY_AUTO_RECALL", true),
    automaticCapture: options.automaticCapture ?? envBoolean("OPENCODE_MEMORY_AUTO_CAPTURE", true),
    automaticDocumentIndex:
      options.automaticDocumentIndex ?? envBoolean("OPENCODE_MEMORY_AUTO_INDEX_DOCUMENTS", true),
    documentIndexDebounceMs,
    automaticOptimize:
      options.automaticOptimize ?? envBoolean("OPENCODE_MEMORY_AUTO_OPTIMIZE", true),
    optimizeDebounceMs,
    sharedSync: options.sharedSync ?? envBoolean("OPENCODE_MEMORY_SHARED_SYNC", true),
    feedbackTracking:
      options.feedbackTracking ?? envBoolean("OPENCODE_MEMORY_FEEDBACK_TRACKING", true),
    minScore,
  };
}

export function isSupportedDocumentPath(path: string): boolean {
  return /\.(?:pdf|md|markdown|html|htm)$/i.test(path);
}

function isModelSwitchInProgressError(error: unknown): boolean {
  return (error instanceof Error ? error.message : String(error)).includes("SWITCH_IN_PROGRESS");
}

function graphCandidatePayload(candidates: GraphExtractionCandidates) {
  return {
    entities: candidates.entities.map((candidate) => ({
      mention: candidate.mention,
      canonical_hint: candidate.canonical_hint,
      entity_type: candidate.entity_type,
      aliases: [...candidate.aliases],
      evidence: candidate.evidence.map((evidence) => ({
        source_unit_id: evidence.source_unit_id,
        quote: evidence.quote,
        occurrence_index: 0,
      })),
      confidence: candidate.confidence,
    })),
    relations: candidates.relations.map((candidate) => ({
      subject_mention: candidate.subject_mention,
      predicate: candidate.predicate,
      object_mention: candidate.object_mention,
      relation_type: candidate.relation_type,
      ...(parseGraphTime(candidate.valid_at) === undefined
        ? {}
        : { valid_at_ms: parseGraphTime(candidate.valid_at) }),
      ...(parseGraphTime(candidate.invalid_at) === undefined
        ? {}
        : { invalid_at_ms: parseGraphTime(candidate.invalid_at) }),
      evidence: candidate.evidence.map((evidence) => ({
        source_unit_id: evidence.source_unit_id,
        quote: evidence.quote,
        occurrence_index: 0,
      })),
      confidence: candidate.confidence,
    })),
  };
}

function parseGraphTime(value: string | null | undefined): number | undefined {
  if (value === undefined || value === null || value.trim() === "") return undefined;
  const parsed = Date.parse(value);
  if (!Number.isFinite(parsed) || parsed < 0 || !Number.isSafeInteger(parsed)) {
    throw new Error(`Graph temporal value is invalid: ${value}`);
  }
  return parsed;
}

function asPluginError(value: unknown): Error {
  return value instanceof Error ? value : new Error(String(value));
}

function graphAuthorizationKey(authorization: GraphAuthorizationContract): string {
  return `${authorization.session_scope_key}\0${authorization.agent_scope_key}`;
}

function graphExtractionJobID(
  sources: readonly GraphSourceBinding[],
  provider: GraphProviderIdentity,
  maxAttempts: number,
): string {
  const bindings = sources
    .map((source) => ({
      source_memory_id: source.source_memory_id,
      source_unit_id: source.source_unit_id,
      content_hash: source.content_hash,
      extraction_revision: source.extraction_revision,
      derived_scope: source.derived_scope,
      origin: source.origin,
      policy_revision: source.policy_revision,
      remote_eligible: source.remote_eligible,
    }))
    .toSorted((left, right) => left.source_memory_id.localeCompare(right.source_memory_id));
  const digest = createHash("sha256")
    .update(JSON.stringify({ bindings, provider, max_attempts: maxAttempts }))
    .digest("hex");
  return `job_${digest.slice(0, 32)}`;
}

function boundedGraphJobError(value: string): string {
  const normalized = value.replace(/[\u0000-\u001f\u007f]/g, " ").trim();
  return [...(normalized || "graph extraction failed")].slice(0, 2_048).join("");
}

async function graphWorkerDelay(ms: number, signal: AbortSignal): Promise<void> {
  if (signal.aborted) return;
  await new Promise<void>((resolveDelay) => {
    const finish = (): void => {
      clearTimeout(timer);
      signal.removeEventListener("abort", finish);
      resolveDelay();
    };
    const timer = setTimeout(finish, ms);
    timer.unref?.();
    signal.addEventListener("abort", finish, { once: true });
  });
}

function envBoolean(name: string, fallback: boolean): boolean {
  const value = process.env[name];
  if (value === undefined || value === "") return fallback;
  if (["1", "true", "yes", "on"].includes(value.toLowerCase())) return true;
  if (["0", "false", "no", "off"].includes(value.toLowerCase())) return false;
  throw new Error(`${name} must be a boolean`);
}

function envNumber(name: string, fallback: number): number {
  const value = process.env[name];
  if (value === undefined || value === "") return fallback;
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) throw new Error(`${name} must be a finite number`);
  return parsed;
}

function result(title: string, value: unknown, metadata: Record<string, unknown>): ToolResult {
  return {
    title,
    output: JSON.stringify(value, null, 2),
    metadata,
  };
}
