import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    listen: vi.fn().mockResolvedValue(() => {}),
    minimize: vi.fn(),
    toggleMaximize: vi.fn(),
    hide: vi.fn(),
  }),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
}));

const invokeMock = vi.mocked(invoke);

type AnyRecord = Record<string, unknown>;

function account(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    fileName: "codex-a.json",
    provider: "codex",
    label: "Codex A",
    disabled: false,
    active: false,
    cooldownUntilMs: null,
    expiresAtMs: null,
    credentialStatus: "active",
    ...overrides,
  };
}

function snapshot(overrides: AnyRecord = {}) {
  return {
    running: true,
    baseUrl: "http://127.0.0.1:8317",
    version: "2.2.9",
    claudeRunning: false,
    codexRunning: false,
    accounts: [] as unknown[],
    routes: [],
    controller: { state: "running", detail: "running" },
    relay: { state: "running", detail: "relay" },
    backend: { state: "healthy", detail: "backend" },
    credentials: { state: "missing", detail: "No active credential" },
    route: { state: "waiting", detail: "route" },
    oauth: { state: "idle", detail: "oauth" },
    claude: { state: "stopped", detail: "claude" },
    codex: { state: "stopped", detail: "codex" },
    activeRequests: 0,
    diagnostics: [],
    skipModelSwitchConfirmation: false,
    ...overrides,
  };
}

function completedLogin() {
  return {
    sessionId: "login-1",
    provider: "codex",
    state: "completed",
    startedAt: new Date().toISOString(),
    resultFileName: "codex-a.json",
    detail: "done",
  };
}

describe("login completion respects the active account", () => {
  let selectCalls: Array<Record<string, unknown>>;
  let launchCalls: number;

  beforeEach(() => {
    selectCalls = [];
    launchCalls = 0;
    vi.useFakeTimers();
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, json: async () => [] }));
    invokeMock.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  async function renderWithLogin(accounts: unknown[]) {
    const withLogin = snapshot({
      accounts,
      login: completedLogin(),
      credentials: accounts.some((entry) => (entry as { active?: boolean }).active)
        ? { state: "selected", detail: "selected" }
        : { state: "missing", detail: "missing" },
    });
    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      switch (command) {
        case "start_gateway":
          return snapshot({ accounts });
        case "gateway_snapshot":
          return withLogin;
        case "select_gateway_account":
          selectCalls.push((args ?? {}) as Record<string, unknown>);
          return { ...withLogin, claudeConfigChanged: false };
        case "get_gateway_account_usage":
          return { fileName: "codex-a.json", provider: "codex", windows: [] };
        case "latest_basiliskos_release":
          return {
            tagName: "v2.2.9",
            name: "Basiliskos v2.2.9",
            body: "",
            publishedAt: "",
            releaseUrl: "https://github.com/LuNexInc/basiliskos/releases/tag/v2.2.9",
          };
        case "launch_hydra_claude":
          launchCalls += 1;
          return { ...withLogin, claudeRunning: true };
        default:
          return {};
      }
    });
    render(<App />);
    // Trigger the 3s snapshot poll; advanceTimersByTimeAsync flushes the
    // microtasks between timer fires so the refresh and login effects settle.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3000);
    });
  }

  it("does not switch the active account when one is already active", async () => {
    await renderWithLogin([account({ active: true })]);
    expect(selectCalls).toHaveLength(0);
    expect(launchCalls).toBe(0);
    expect(screen.getByText(/Choose Use account to route/)).toBeInTheDocument();
  });

  it("activates and opens the window for the first account only", async () => {
    await renderWithLogin([account({ active: false })]);
    expect(selectCalls).toHaveLength(1);
    expect(selectCalls[0].fileName).toBe("codex-a.json");
    expect(launchCalls).toBe(1);
    expect(screen.getByText(/Account authorized and selected/)).toBeInTheDocument();
  });
});
