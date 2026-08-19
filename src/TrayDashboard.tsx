import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  AppWindow,
  CircleStop,
  ExternalLink,
  LoaderCircle,
  Play,
  Power,
  X,
} from "lucide-react";
import brandArt from "./assets/basiliskos-mark.png";
import { contextWindowLabel, messageFrom, statusTone, thinkingLabel } from "./ui";
import { APP_VERSION } from "./App";

type Provider = "claude" | "codex" | "xai" | "kimi" | "deepseek" | "antigravity";

type Account = {
  fileName: string;
  provider: Provider;
  email?: string;
  label: string;
  active: boolean;
};

type UsageWindow = {
  label: string;
  usedPercent: number;
  remainingPercent: number;
  known: boolean;
};

type AccountUsage = {
  fileName: string;
  provider: Provider;
  windows: UsageWindow[];
};

type ComponentStatus = {
  state: string;
  detail: string;
};

type ProviderRoute = {
  provider: Provider;
  selectedModel: string;
  selectedModelLabel: string;
  thinking: string;
  contextWindow?: number;
};

type Snapshot = {
  running: boolean;
  version: string;
  claudeRunning: boolean;
  codexRunning?: boolean;
  accounts: Account[];
  routes: ProviderRoute[];
  autoFailover?: { fromLabel: string; toLabel: string; atMs: number };
  relay: ComponentStatus;
  backend: ComponentStatus;
  claude: ComponentStatus;
  codex?: ComponentStatus;
};

type ActiveServiceIdentities = {
  relayEmail?: string;
  codexCliEmail?: string;
  grokCliEmail?: string;
};

type FuelTone = "full" | "mid" | "low" | "critical" | "unknown";

const CORE_SEGMENTS = 18;
const PREVIEW_TRAY =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).has("tray");

const PREVIEW_SNAPSHOT: Snapshot = {
  running: true,
  version: "preview",
  claudeRunning: true,
  codexRunning: true,
  accounts: [
    {
      fileName: "claude-preview.json",
      provider: "claude",
      email: "charles@preview.local",
      label: "Claude primary",
      active: true,
    },
    {
      fileName: "codex-preview.json",
      provider: "codex",
      email: "codex@preview.local",
      label: "Codex worker",
      active: false,
    },
    {
      fileName: "grok-preview.json",
      provider: "xai",
      email: "grok@preview.local",
      label: "Grok worker",
      active: false,
    },
  ],
  routes: [
    {
      provider: "claude",
      selectedModel: "claude-fable-5",
      selectedModelLabel: "Claude Fable 5",
      thinking: "high",
      contextWindow: 200000,
    },
  ],
  relay: { state: "healthy", detail: "Preview relay" },
  backend: { state: "healthy", detail: "Preview link" },
  claude: { state: "running", detail: "Preview Claude window" },
  codex: { state: "running", detail: "Preview ChatGPT window" },
};

const PREVIEW_IDENTITIES: ActiveServiceIdentities = {
  relayEmail: "charles@preview.local",
  codexCliEmail: "codex@preview.local",
  grokCliEmail: "grok@preview.local",
};

const PREVIEW_USAGE: Record<string, AccountUsage> = {
  "claude-preview.json": {
    fileName: "claude-preview.json",
    provider: "claude",
    windows: [{ label: "5h", usedPercent: 28, remainingPercent: 72, known: true }],
  },
  "codex-preview.json": {
    fileName: "codex-preview.json",
    provider: "codex",
    windows: [{ label: "5h", usedPercent: 61, remainingPercent: 39, known: true }],
  },
  "grok-preview.json": {
    fileName: "grok-preview.json",
    provider: "xai",
    windows: [{ label: "5h", usedPercent: 91, remainingPercent: 9, known: true }],
  },
};

function fuelTone(percent?: number): FuelTone {
  if (percent === undefined || Number.isNaN(percent)) return "unknown";
  if (percent <= 12) return "critical";
  if (percent <= 30) return "low";
  if (percent <= 60) return "mid";
  return "full";
}

function ReactorCore({
  percent,
  label,
}: {
  percent?: number;
  label: string;
}) {
  const tone = fuelTone(percent);
  const lit =
    percent === undefined
      ? 0
      : Math.max(0, Math.min(CORE_SEGMENTS, Math.round((percent / 100) * CORE_SEGMENTS)));
  const aria =
    percent === undefined
      ? `${label}: usage unrecorded`
      : `${label}: ${Math.round(percent)} percent fuel remaining`;

  return (
    <div className={`tray-fuel tone-${tone}`} data-tone={tone}>
      <div
        className="tray-core"
        role="img"
        aria-label={aria}
        title={aria}
      >
        {Array.from({ length: CORE_SEGMENTS }, (_, index) => (
          <span key={index} className={index < lit ? "lit" : ""} />
        ))}
      </div>
      <span className="tray-fuel-readout">
        {percent === undefined ? "UNRECORDED" : `${Math.round(percent)}% FUEL`}
      </span>
    </div>
  );
}

function primaryUsagePercent(usage?: AccountUsage) {
  if (!usage?.windows?.length) return undefined;
  const known = usage.windows.filter((window) => window.known);
  if (known.length === 0) return undefined;
  return known[0].remainingPercent;
}

export default function TrayDashboard() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [activeIdentities, setActiveIdentities] = useState<ActiveServiceIdentities | null>(null);
  const [usageByAccount, setUsageByAccount] = useState<Record<string, AccountUsage | undefined>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState("Loading…");
  const [isError, setIsError] = useState(false);
  const lastFailoverAtMs = useRef<number | null>(null);

  useEffect(() => {
    document.documentElement.classList.add("tray-dashboard");
    document.body.classList.add("tray-dashboard");
    return () => {
      document.documentElement.classList.remove("tray-dashboard");
      document.body.classList.remove("tray-dashboard");
    };
  }, []);

  const refresh = useCallback(async () => {
    if (PREVIEW_TRAY) {
      setSnapshot(PREVIEW_SNAPSHOT);
      setActiveIdentities(PREVIEW_IDENTITIES);
      setUsageByAccount(PREVIEW_USAGE);
      setMessage("Claude window open");
      setIsError(false);
      return;
    }
    try {
      const [next, identities] = await Promise.all([
        invoke<Snapshot>("gateway_snapshot"),
        invoke<ActiveServiceIdentities>("active_service_identities"),
      ]);
      setSnapshot(next);
      setActiveIdentities(identities);
      setIsError(false);
      if (!next.running) {
        setMessage("Relay stopped");
      } else if (!next.accounts.some((account) => account.active)) {
        setMessage("No active account");
      } else {
        setMessage(next.claudeRunning ? "Claude window open" : "Relay ready");
      }
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    }
  }, []);

  const refreshUsage = useCallback(async (fileNames: string[]) => {
    if (PREVIEW_TRAY) {
      setUsageByAccount(PREVIEW_USAGE);
      return;
    }
    const unique = [...new Set(fileNames.filter(Boolean))];
    if (unique.length === 0) return;
    await Promise.all(
      unique.map(async (fileName) => {
        try {
          const data = await invoke<AccountUsage>("get_gateway_account_usage", { fileName });
          setUsageByAccount((current) => ({ ...current, [fileName]: data }));
        } catch {
          setUsageByAccount((current) => ({ ...current, [fileName]: undefined }));
        }
      }),
    );
  }, []);

  useEffect(() => {
    void refresh();
    const interval = window.setInterval(() => void refresh(), 3000);
    return () => window.clearInterval(interval);
  }, [refresh]);

  useEffect(() => {
    const failover = snapshot?.autoFailover;
    if (!failover) return;
    if (lastFailoverAtMs.current === failover.atMs) return;
    lastFailoverAtMs.current = failover.atMs;
    setMessage(`${failover.fromLabel} was rate-limited; switched to ${failover.toLabel}.`);
    setIsError(false);
  }, [snapshot?.autoFailover]);

  useEffect(() => {
    if (!snapshot) return;
    const codex = snapshot.accounts.find(
      (account) => !!account.email && account.email === activeIdentities?.codexCliEmail,
    );
    const grok = snapshot.accounts.find(
      (account) => !!account.email && account.email === activeIdentities?.grokCliEmail,
    );
    const active = snapshot.accounts.find((account) => account.active);
    void refreshUsage(
      [active?.fileName, codex?.fileName, grok?.fileName].filter((value): value is string => !!value),
    );
  }, [snapshot, activeIdentities, refreshUsage]);

  const active = snapshot?.accounts.find((account) => account.active);
  const activeRoute = snapshot?.routes.find((route) => route.provider === active?.provider);
  const codexCliAccount = snapshot?.accounts.find(
    (account) => !!account.email && account.email === activeIdentities?.codexCliEmail,
  );
  const grokCliAccount = snapshot?.accounts.find(
    (account) => !!account.email && account.email === activeIdentities?.grokCliEmail,
  );
  const activeUsage = primaryUsagePercent(usageByAccount[active?.fileName ?? ""]);
  const codexUsage = primaryUsagePercent(usageByAccount[codexCliAccount?.fileName ?? ""]);
  const grokUsage = primaryUsagePercent(usageByAccount[grokCliAccount?.fileName ?? ""]);
  const systemsLive = statusTone(snapshot?.relay) === "healthy" && statusTone(snapshot?.backend) === "healthy";

  async function startOrStop() {
    if (PREVIEW_TRAY) {
      setSnapshot((current) =>
        current
          ? {
              ...current,
              running: !current.running,
              relay: {
                state: current.running ? "stopped" : "healthy",
                detail: current.running ? "Preview stopped" : "Preview relay",
              },
            }
          : current,
      );
      setMessage(snapshot?.running ? "Relay stopped" : "Relay started");
      return;
    }
    setBusy("power");
    try {
      const action = snapshot?.running ? "stop_gateway" : "start_gateway";
      setSnapshot(await invoke<Snapshot>(action));
      setMessage(action === "start_gateway" ? "Relay started" : "Relay stopped");
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function openClaude() {
    if (PREVIEW_TRAY) {
      setSnapshot((current) =>
        current
          ? {
              ...current,
              claudeRunning: true,
              claude: { state: "running", detail: "Preview Claude window" },
            }
          : current,
      );
      setMessage("Opened Basiliskos Claude");
      return;
    }
    setBusy("claude");
    try {
      setSnapshot(await invoke<Snapshot>("launch_hydra_claude"));
      setMessage("Opened Basiliskos Claude");
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function closeClaude() {
    if (PREVIEW_TRAY) {
      setSnapshot((current) =>
        current
          ? {
              ...current,
              claudeRunning: false,
              claude: { state: "stopped", detail: "Preview Claude closed" },
            }
          : current,
      );
      setMessage("Closed Basiliskos Claude");
      return;
    }
    setBusy("claude");
    try {
      setSnapshot(await invoke<Snapshot>("stop_hydra_claude"));
      setMessage("Closed Basiliskos Claude");
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function openFull() {
    if (PREVIEW_TRAY) {
      setMessage("Open full window (preview)");
      return;
    }
    try {
      await invoke("show_main_window");
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    }
  }

  async function dismiss() {
    if (PREVIEW_TRAY) {
      setMessage("Dismiss (preview)");
      return;
    }
    try {
      await invoke("hide_tray_dashboard");
    } catch {
      // Best-effort dismiss.
    }
  }

  async function quit() {
    if (PREVIEW_TRAY) {
      setMessage("Quit (preview)");
      return;
    }
    setBusy("quit");
    try {
      await invoke("quit_basiliskos");
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
      setBusy(null);
    }
  }

  return (
    <main className={`tray-shell${systemsLive ? " systems-live" : ""}`} aria-label="Basiliskos tray dashboard">
      <div className="tray-starfield" aria-hidden="true" />
      <div className="tray-scan" aria-hidden="true" />

      <header className="tray-topbar" data-tauri-drag-region>
        <div className="tray-brand">
          <div className="tray-mark">
            <img src={brandArt} alt="" />
          </div>
          <div>
            <h1>BasiliskOS</h1>
            <p>Relay</p>
          </div>
        </div>
        <div className="tray-top-meta">
          <span className={`tray-core-badge${systemsLive ? " live" : ""}`}>
            <i aria-hidden="true" />
            {systemsLive ? "CORE ONLINE" : "CORE STANDBY"}
          </span>
          <button type="button" className="tray-icon-btn" aria-label="Close dashboard" onClick={() => void dismiss()}>
            <X size={14} />
          </button>
        </div>
      </header>

      <section className="tray-health" aria-label="Health">
        <span className={statusTone(snapshot?.relay)} title={snapshot?.relay.detail}>
          <i aria-hidden="true" /> Relay · {snapshot?.relay.state ?? "…"}
        </span>
        <span className={statusTone(snapshot?.backend)} title={snapshot?.backend.detail}>
          <i aria-hidden="true" /> Link · {snapshot?.backend.state ?? "…"}
        </span>
        <span className={statusTone(snapshot?.claude)} title={snapshot?.claude.detail}>
          <i aria-hidden="true" /> Claude · {snapshot?.claude.state ?? "…"}
        </span>
        <span className={statusTone(snapshot?.codex)} title={snapshot?.codex?.detail}>
          <i aria-hidden="true" /> ChatGPT · {snapshot?.codex?.state ?? "…"}
        </span>
      </section>

      <section className="tray-services" aria-label="Current connection">
        <article className="tray-service">
          <div className="tray-service-head">
            <span className="eyebrow">Claude Code</span>
            <span className="tray-service-tag">PRIMARY</span>
          </div>
          <h2>{active && activeRoute ? activeRoute.selectedModelLabel : "No account"}</h2>
          <p>
            {active && activeRoute
              ? `${active.label} · Thinking ${thinkingLabel(activeRoute.thinking)}${
                  contextWindowLabel(activeRoute.contextWindow)
                    ? ` · ${contextWindowLabel(activeRoute.contextWindow)}`
                    : ""
                }`
              : "Serve an account in the full window"}
          </p>
          <ReactorCore percent={activeUsage} label="Claude Code" />
        </article>

        <article className="tray-service">
          <div className="tray-service-head">
            <span className="eyebrow">ChatGPT</span>
            <span className="tray-service-tag">{snapshot?.codexRunning ? "OPEN" : "CLOSED"}</span>
          </div>
          <h2>{active && activeRoute ? activeRoute.selectedModelLabel : "No account"}</h2>
          <p>
            {active
              ? `${active.label} · ${snapshot?.codexRunning ? "Window open" : "Window closed"}`
              : "Serve from full window"}
          </p>
          <ReactorCore percent={activeUsage} label="ChatGPT" />
        </article>

        <article className="tray-service">
          <div className="tray-service-head">
            <span className="eyebrow">Codex CLI</span>
            <span className="tray-service-tag">WORKER</span>
          </div>
          <h2>{codexCliAccount ? codexCliAccount.label : "Not set"}</h2>
          <p>{codexCliAccount ? codexCliAccount.email ?? "Real codex command" : "Serve from full window"}</p>
          <ReactorCore percent={codexUsage} label="Codex CLI" />
        </article>

        <article className="tray-service">
          <div className="tray-service-head">
            <span className="eyebrow">Grok CLI</span>
            <span className="tray-service-tag">WORKER</span>
          </div>
          <h2>{grokCliAccount ? grokCliAccount.label : "Not set"}</h2>
          <p>{grokCliAccount ? grokCliAccount.email ?? "Real grok command" : "Serve from full window"}</p>
          <ReactorCore percent={grokUsage} label="Grok CLI" />
        </article>
      </section>

      <section className="tray-actions" aria-label="Quick actions">
        <button type="button" className="primary" onClick={() => void startOrStop()} disabled={busy !== null}>
          {busy === "power" ? (
            <LoaderCircle className="spin" size={15} />
          ) : snapshot?.running ? (
            <CircleStop size={15} />
          ) : (
            <Play size={15} />
          )}
          {snapshot?.running ? "Stop relay" : "Start relay"}
        </button>
        {snapshot?.claudeRunning ? (
          <button type="button" onClick={() => void closeClaude()} disabled={busy !== null}>
            {busy === "claude" ? <LoaderCircle className="spin" size={15} /> : <AppWindow size={15} />}
            Close Claude
          </button>
        ) : (
          <button
            type="button"
            onClick={() => void openClaude()}
            disabled={busy !== null || !active || snapshot?.backend.state !== "healthy"}
          >
            {busy === "claude" ? <LoaderCircle className="spin" size={15} /> : <AppWindow size={15} />}
            Open Claude
          </button>
        )}
        <button type="button" className="secondary" onClick={() => void openFull()} disabled={busy !== null}>
          <ExternalLink size={15} />
          Open full window
        </button>
        <button type="button" className="danger" onClick={() => void quit()} disabled={busy !== null}>
          {busy === "quit" ? <LoaderCircle className="spin" size={15} /> : <Power size={15} />}
          Quit Basiliskos
        </button>
      </section>

      <footer className="tray-foot">
        <p className={isError ? "error-message" : ""} aria-live="polite">
          {message}
        </p>
        <span>
          v{APP_VERSION} · CLIProxyAPI {snapshot?.version ?? "…"}
        </span>
      </footer>
    </main>
  );
}
