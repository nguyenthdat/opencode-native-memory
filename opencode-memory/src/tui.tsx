/** @jsxImportSource @opentui/solid */
import { fileURLToPath } from "node:url";
import { basename, dirname, resolve } from "node:path";
import type { TuiPlugin, TuiPluginApi, TuiPluginModule } from "@opencode-ai/plugin/tui";
import { createSignal } from "solid-js";
import type {
  MemoryPluginHealth,
  MemoryPluginHealthStatus,
  NativeMemoryStatus,
} from "./contracts.js";
import { acquireNativeMemoryClient, type NativeMemoryClientLease } from "./daemon-client.js";
import { buildMemoryStatusResponse } from "./plugin-health.js";

const PLUGIN_ID = "@nguyenthdat/opencode-memory";
const DEFAULT_REFRESH_INTERVAL_MS = 30_000;

export interface MemoryTuiHealth {
  status: MemoryPluginHealthStatus | "checking";
  ready: boolean;
  message: string;
}

interface MemoryTuiDependencies {
  acquireClient?: (root: string, worktree: string) => Promise<NativeMemoryClientLease>;
  refreshIntervalMs?: number;
}

export function createMemoryTui(root: string, dependencies: MemoryTuiDependencies = {}): TuiPlugin {
  return async (api, options) => {
    if (options?.enabled === false) return;

    const acquireClient = dependencies.acquireClient ?? acquireNativeMemoryClient;
    const refreshIntervalMs = dependencies.refreshIntervalMs ?? DEFAULT_REFRESH_INTERVAL_MS;
    const worktree = api.state.path.worktree || api.state.path.directory;
    const lease = await acquireClient(root, worktree);
    const [health, setHealth] = createSignal<MemoryTuiHealth>({
      status: "checking",
      ready: false,
      message: "Checking native memory daemon",
    });
    let inFlight: Promise<MemoryTuiHealth> | undefined;

    const checkHealth = async (): Promise<MemoryTuiHealth> => {
      try {
        const status = await lease.client.request<NativeMemoryStatus>(
          "status",
          {},
          api.lifecycle.signal,
        );
        const next = tuiHealth(
          buildMemoryStatusResponse(
            { status: "fulfilled", value: status },
            { status: "fulfilled", value: undefined },
            { status: "fulfilled", value: undefined },
          ).plugin_health,
        );
        if (!api.lifecycle.signal.aborted) setHealth(next);
        return next;
      } catch (error) {
        if (api.lifecycle.signal.aborted) return health();
        const next = tuiHealth(
          buildMemoryStatusResponse(
            { status: "rejected", reason: error },
            { status: "fulfilled", value: undefined },
            { status: "fulfilled", value: undefined },
          ).plugin_health,
        );
        setHealth(next);
        return next;
      }
    };

    const refresh = (): Promise<MemoryTuiHealth> => {
      if (inFlight) return inFlight;
      const operation = checkHealth();
      inFlight = operation;
      void operation.finally(() => {
        if (inFlight === operation) inFlight = undefined;
      });
      return operation;
    };

    api.slots.register({
      order: 120,
      slots: {
        app_bottom() {
          return <MemoryHealthBadge api={api} health={health} />;
        },
      },
    });

    const unregisterCommands = api.keymap.registerLayer({
      commands: [
        {
          name: "memory.health.refresh",
          title: "Check memory health",
          desc: "Refresh the native memory daemon and project health status",
          category: "Memory",
          namespace: "palette",
          suggested: true,
          slashName: "memory-health",
          run() {
            void refresh().then((next) => {
              api.ui.toast({
                variant: healthVariant(next.status),
                title: "Memory health",
                message: next.message,
                duration: 5_000,
              });
            });
          },
        },
      ],
      bindings: [],
    });

    const interval = setInterval(() => void refresh(), refreshIntervalMs);
    interval.unref?.();
    api.lifecycle.onDispose(async () => {
      clearInterval(interval);
      unregisterCommands();
      await lease.release();
    });
    void refresh();
  };
}

export function memoryHealthText(health: MemoryTuiHealth): string {
  return `Memory: ${health.status.replace("_", " ")}`;
}

function MemoryHealthBadge(props: { api: TuiPluginApi; health: () => MemoryTuiHealth }) {
  const color = () => {
    switch (props.health().status) {
      case "healthy":
        return props.api.theme.current.success;
      case "degraded":
        return props.api.theme.current.warning;
      case "unavailable":
        return props.api.theme.current.error;
      case "checking":
        return props.api.theme.current.textMuted;
    }
  };
  return (
    <box paddingLeft={1} paddingRight={1}>
      <text fg={color()}>{memoryHealthText(props.health())}</text>
    </box>
  );
}

function tuiHealth(health: MemoryPluginHealth): MemoryTuiHealth {
  return {
    status: health.status,
    ready: health.ready,
    message:
      health.issues[0]?.message ??
      (health.ready ? "Native memory backend is ready" : "Native memory backend is unavailable"),
  };
}

function healthVariant(
  status: MemoryTuiHealth["status"],
): "info" | "success" | "warning" | "error" {
  switch (status) {
    case "healthy":
      return "success";
    case "degraded":
      return "warning";
    case "unavailable":
      return "error";
    case "checking":
      return "info";
  }
}

const moduleDirectory = dirname(fileURLToPath(import.meta.url));
const packageRoot =
  basename(moduleDirectory) === "dist"
    ? resolve(moduleDirectory, "..")
    : resolve(moduleDirectory, "../..");

const memoryTui = {
  id: PLUGIN_ID,
  tui: createMemoryTui(packageRoot),
} satisfies TuiPluginModule;

export default memoryTui;
