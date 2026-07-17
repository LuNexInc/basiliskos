import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  AppWindow,
  AlertTriangle,
  BellDot,
  Check,
  CircleStop,
  Download,
  FolderOpen,
  LoaderCircle,
  LogIn,
  Maximize2,
  Minus,
  Pencil,
  Play,
  Power,
  RefreshCw,
  ShieldCheck,
  Trash2,
  X,
} from "lucide-react";
import brandArt from "./assets/basiliskos-mark.png";
import "./App.css";

type Provider = "claude" | "codex" | "xai" | "kimi";

type Account = {
  fileName: string;
  provider: Provider;
  email?: string;
  label: string;
  disabled: boolean;
  active: boolean;
};

type UsageWindow = {
  label: string;
  usedPercent: number;
  remainingPercent: number;
};

type AccountUsage = {
  fileName: string;
  provider: Provider;
  windows: UsageWindow[];
};

type UsageLoadState = {
  loading: boolean;
  data?: AccountUsage;
  error?: string;
};

type Snapshot = {
  running: boolean;
  baseUrl: string;
  version: string;
  claudeRunning: boolean;
  accounts: Account[];
  activeAccount?: string;
  routes: ProviderRoute[];
  controller: ComponentStatus;
  relay: ComponentStatus;
  backend: ComponentStatus;
  credentials: ComponentStatus;
  route: ComponentStatus;
  oauth: ComponentStatus;
  claude: ComponentStatus;
  backendExitReason?: string;
  activeRequests: number;
  diagnostics: DiagnosticEvent[];
  login?: ProviderLoginStatus;
  skipModelSwitchConfirmation: boolean;
};

export type ComponentStatus = {
  state: string;
  detail: string;
};

export type DiagnosticEvent = {
  timestamp: string;
  correlationId?: string;
  code: string;
  severity: string;
  message: string;
  httpStatus?: number;
  provider?: string;
};

type ProviderLoginStatus = {
  sessionId: string;
  provider: Provider;
  state: "waiting" | "completed" | "failed" | "cancelled";
  startedAt: string;
  resultFileName?: string;
  detail: string;
};

type RouteModelOption = {
  id: string;
  label: string;
  thinkingLevels: string[];
};

type ProviderRoute = {
  provider: Provider;
  selectedModel: string;
  selectedModelLabel: string;
  thinking: string;
  contextWindow?: number;
  modelOptions: RouteModelOption[];
};

type ProviderLoginLaunch = {
  sessionId: string;
  authorizationUrl: string;
  userCode?: string;
};

type ReleaseAsset = {
  name: string;
  browser_download_url: string;
};

type Release = {
  tagName: string;
  name: string;
  publishedAt: string;
  body: string;
  installerUrl?: string;
  releaseUrl?: string;
};

type LatestPublishedRelease = {
  tagName: string;
  releaseUrl: string;
};

type AppView = "console" | "changes";

const APP_VERSION = "1.1.17";
const RELEASES_URL = "https://api.github.com/repos/LuNexInc/basiliskos/releases?per_page=12";

const PROVIDERS: Array<{ id: Provider; label: string; detail: string }> = [
  { id: "claude", label: "Claude", detail: "Claude OAuth" },
  { id: "codex", label: "Codex", detail: "ChatGPT / Codex OAuth" },
  { id: "xai", label: "Grok", detail: "Grok Build OAuth" },
  { id: "kimi", label: "Kimi", detail: "Kimi Code OAuth" },
];

function messageFrom(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function thinkingLabel(value: string) {
  const labels: Record<string, string> = {
    auto: "Auto",
    none: "Off",
    low: "Low",
    medium: "Medium",
    high: "High",
    xhigh: "Extra high",
    max: "Maximum",
    ultra: "Ultra",
  };
  return labels[value] ?? value;
}

function contextWindowLabel(tokens?: number) {
  if (!tokens) return null;
  return `${Math.round(tokens / 1000)}K context`;
}

export function isNewerVersion(candidate: string, current: string) {
  const parts = (value: string) => value.replace(/^v/i, "").split(".").map((part) => Number.parseInt(part, 10));
  const candidateParts = parts(candidate);
  const currentParts = parts(current);
  if ([...candidateParts, ...currentParts].some((part) => Number.isNaN(part))) return false;
  const length = Math.max(candidateParts.length, currentParts.length);
  for (let index = 0; index < length; index += 1) {
    const difference = (candidateParts[index] ?? 0) - (currentParts[index] ?? 0);
    if (difference !== 0) return difference > 0;
  }
  return false;
}

function parseReleases(payload: unknown): Release[] {
  if (!Array.isArray(payload)) return [];
  return payload.flatMap((item) => {
    if (!item || typeof item !== "object") return [];
    const record = item as Record<string, unknown>;
    if (record.draft === true || record.prerelease === true || typeof record.tag_name !== "string") return [];
    const assets = Array.isArray(record.assets) ? record.assets as ReleaseAsset[] : [];
    const installerUrl = assets.find((asset) => asset?.name?.endsWith("_x64-setup.exe"))?.browser_download_url;
    return [{
      tagName: record.tag_name,
      name: typeof record.name === "string" ? record.name : record.tag_name,
      publishedAt: typeof record.published_at === "string" ? record.published_at : "",
      body: typeof record.body === "string" ? record.body : "No release notes were provided.",
      installerUrl,
    }];
  });
}

export function statusTone(status?: ComponentStatus) {
  if (!status) return "offline";
  if (["running", "healthy", "selected", "ready", "completed"].includes(status.state)) {
    return "healthy";
  }
  if (["starting", "waiting"].includes(status.state)) return "pending";
  if (["degraded", "failed"].includes(status.state)) return "degraded";
  return "offline";
}

export function StatusBadge({ label, status }: { label: string; status?: ComponentStatus }) {
  return <span className={statusTone(status)} title={status?.detail}><i aria-hidden="true" />{label} · {status?.state ?? "unknown"}</span>;
}

export function DiagnosticEventList({ events }: { events: DiagnosticEvent[] }) {
  if (events.length === 0) return <p className="no-events">No failures recorded in this session.</p>;
  return events.map((event) => (
    <article className={`diagnostic-event ${event.severity}`} key={`${event.timestamp}-${event.code}-${event.correlationId ?? "local"}`}>
      <AlertTriangle size={15} aria-hidden="true" />
      <div><strong>{event.code}</strong><p>{event.message}</p></div>
      <time dateTime={event.timestamp}>{new Date(event.timestamp).toLocaleTimeString()}</time>
    </article>
  ));
}

export default function App() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [provider, setProvider] = useState<Provider>("codex");
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState("Starting Basiliskos…");
  const [isError, setIsError] = useState(false);
  const [usageByAccount, setUsageByAccount] = useState<Record<string, UsageLoadState>>({});
  const [editingAccount, setEditingAccount] = useState<string | null>(null);
  const [draftName, setDraftName] = useState("");
  const [showDiagnostics, setShowDiagnostics] = useState(false);
  const [view, setView] = useState<AppView>("console");
  const [releases, setReleases] = useState<Release[]>([]);
  const [checkingUpdates, setCheckingUpdates] = useState(false);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const handledLogin = useRef<string | null>(null);
  const [accountSwitchConfirm, setAccountSwitchConfirm] = useState<{
    open: boolean;
    account: Account | null;
    dontShowAgain: boolean;
  }>({ open: false, account: null, dontShowAgain: false });

  const refresh = useCallback(async (quiet = false) => {
    try {
      const next = await invoke<Snapshot>("gateway_snapshot");
      setSnapshot(next);
      if (!quiet) {
        setMessage("Status refreshed");
        setIsError(false);
      }
      return next;
    } catch (error) {
      if (!quiet) {
        setMessage(messageFrom(error));
        setIsError(true);
      }
      return null;
    }
  }, []);

  const checkForUpdates = useCallback(async (quiet = false) => {
    setCheckingUpdates(true);
    try {
      const response = await fetch(RELEASES_URL, { headers: { Accept: "application/vnd.github+json" } });
      const next = response.ok
        ? parseReleases(await response.json())
        : await invoke<LatestPublishedRelease>("latest_basiliskos_release").then((release) => [{
          tagName: release.tagName,
          name: `Basiliskos ${release.tagName}`,
          publishedAt: "",
          body: "Release details are available on GitHub.",
          releaseUrl: release.releaseUrl,
        }]);
      setReleases(next);
      setUpdateError(null);
      const latest = next.find((release) => isNewerVersion(release.tagName, APP_VERSION));
      if (latest && !quiet) {
        setMessage(`${latest.name} is ready to download.`);
        setIsError(false);
      } else if (!quiet) {
        setMessage("Basiliskos is up to date.");
        setIsError(false);
      }
    } catch (error) {
      const detail = messageFrom(error);
      setUpdateError(detail);
      if (!quiet) {
        setMessage(detail);
        setIsError(true);
      }
    } finally {
      setCheckingUpdates(false);
    }
  }, []);

  useEffect(() => {
    void (async () => {
      setBusy("start");
      try {
        const next = await invoke<Snapshot>("start_gateway");
        if (next.activeAccount) {
          const launched = await invoke<Snapshot>("launch_hydra_claude");
          setSnapshot(launched);
          setMessage("Relay ready. Opened the separate Basiliskos Claude window.");
        } else {
          setSnapshot(next);
          setMessage("Relay ready. Add or choose an account.");
        }
        setIsError(false);
      } catch (error) {
        setMessage(messageFrom(error));
        setIsError(true);
      } finally {
        setBusy(null);
      }
    })();
  }, []);

  useEffect(() => {
    void checkForUpdates(true);
  }, [checkForUpdates]);

  useEffect(() => {
    const interval = window.setInterval(() => void refresh(true), 3000);
    return () => window.clearInterval(interval);
  }, [refresh]);

  useEffect(() => {
    const login = snapshot?.login;
    if (!login || handledLogin.current === login.sessionId || login.state === "waiting") return;
    handledLogin.current = login.sessionId;
    if (login.state !== "completed" || !login.resultFileName) {
      setMessage(login.detail);
      setIsError(login.state === "failed");
      return;
    }
    void (async () => {
      setBusy("complete-login");
      try {
        const selected = await invoke<Snapshot>("select_gateway_account", {
          fileName: login.resultFileName,
        });
        const next = selected.claudeRunning
          ? selected
          : await invoke<Snapshot>("launch_hydra_claude");
        setSnapshot(next);
        setProvider(login.provider);
        setMessage("Account authorized and selected. The isolated Basiliskos Claude window is ready.");
        setIsError(false);
      } catch (error) {
        setMessage(messageFrom(error));
        setIsError(true);
      } finally {
        setBusy(null);
      }
    })();
  }, [snapshot?.login]);

  const accounts = useMemo(
    () => snapshot?.accounts.filter((account) => account.provider === provider) ?? [],
    [provider, snapshot],
  );
  const accountFilesKey = accounts.map((account) => account.fileName).join("\u0000");
  const active = snapshot?.accounts.find((account) => account.active);
  const activeRoute = snapshot?.routes.find((route) => route.provider === active?.provider);
  const selectedModel = activeRoute?.modelOptions.find(
    (model) => model.id === activeRoute.selectedModel,
  );
  const loginWaiting = snapshot?.login?.state === "waiting";
  const providerCounts = PROVIDERS.map((item) => ({
    ...item,
    count: snapshot?.accounts.filter((account) => account.provider === item.id).length ?? 0,
  }));

  const refreshUsage = useCallback(async (fileNames: string[]) => {
    if (fileNames.length === 0) return;
    setUsageByAccount((current) => {
      const next = { ...current };
      for (const fileName of fileNames) {
        next[fileName] = { ...next[fileName], loading: true, error: undefined };
      }
      return next;
    });
    await Promise.all(fileNames.map(async (fileName) => {
      try {
        const data = await invoke<AccountUsage>("get_gateway_account_usage", { fileName });
        setUsageByAccount((current) => ({
          ...current,
          [fileName]: { loading: false, data },
        }));
      } catch (error) {
        setUsageByAccount((current) => ({
          ...current,
          [fileName]: { loading: false, error: messageFrom(error) },
        }));
      }
    }));
  }, []);

  useEffect(() => {
    const fileNames = accountFilesKey ? accountFilesKey.split("\u0000") : [];
    void refreshUsage(fileNames);
    const interval = window.setInterval(() => void refreshUsage(fileNames), 5 * 60_000);
    return () => window.clearInterval(interval);
  }, [accountFilesKey, refreshUsage]);

  async function startOrStop() {
    const action = snapshot?.running ? "stop_gateway" : "start_gateway";
    setBusy("power");
    try {
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

  async function selectAccount(account: Account) {
    const restartClaude = snapshot?.claudeRunning === true;
    setBusy(account.fileName);
    try {
      let next = await invoke<Snapshot>("select_gateway_account", {
        fileName: account.fileName,
      });
      if (restartClaude) {
        await invoke<Snapshot>("stop_hydra_claude");
        next = await invoke<Snapshot>("launch_hydra_claude");
      } else if (!next.claudeRunning) {
        next = await invoke<Snapshot>("launch_hydra_claude");
      }
      setSnapshot(next);
      setMessage(`${account.label} is now serving the separate Basiliskos Claude window`);
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function removeAccount(account: Account) {
    if (!window.confirm(`Remove ${account.label} from Basiliskos?`)) return;
    setBusy(account.fileName);
    try {
      setSnapshot(
        await invoke<Snapshot>("remove_gateway_account", {
          fileName: account.fileName,
        }),
      );
      setMessage("Account removed from this device");
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  function beginRename(account: Account) {
    setEditingAccount(account.fileName);
    setDraftName(account.label);
  }

  function cancelRename() {
    setEditingAccount(null);
    setDraftName("");
  }

  async function renameAccount(account: Account) {
    const name = draftName.trim();
    if (!name) {
      setMessage("Profile name cannot be empty");
      setIsError(true);
      return;
    }
    setBusy(`rename:${account.fileName}`);
    try {
      setSnapshot(await invoke<Snapshot>("rename_gateway_account", {
        fileName: account.fileName,
        name,
      }));
      setEditingAccount(null);
      setDraftName("");
      setMessage(`Renamed profile to ${name}`);
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function updateRoute(model: string, thinking: string) {
    if (!active) return;
    setBusy("route");
    setMessage("Updating the Basiliskos route…");
    setIsError(false);
    try {
      const next = await invoke<Snapshot>("set_gateway_route", {
        provider: active.provider,
        model,
        thinking,
      });
      setSnapshot(next);
      const route = next.routes.find((item) => item.provider === active.provider);
      setMessage(
        route
          ? `Basiliskos now routes to ${route.selectedModelLabel} · ${thinkingLabel(route.thinking)}. Applies to the next request.`
          : "Basiliskos route updated",
      );
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  function chooseModel(model: string) {
    const option = activeRoute?.modelOptions.find((item) => item.id === model);
    const nextThinking =
      activeRoute?.thinking === "auto" || option?.thinkingLevels.includes(activeRoute?.thinking ?? "")
        ? activeRoute?.thinking ?? "auto"
        : "auto";
    void updateRoute(model, nextThinking);
  }

  function requestAccountSelection(account: Account) {
    if (snapshot?.claudeRunning && !snapshot.skipModelSwitchConfirmation) {
      setAccountSwitchConfirm({ open: true, account, dontShowAgain: false });
      return;
    }
    void selectAccount(account);
  }

  async function confirmAccountSwitch() {
    const { account, dontShowAgain } = accountSwitchConfirm;
    setAccountSwitchConfirm((prev) => ({ ...prev, open: false }));
    if (!account) return;
    if (dontShowAgain) {
      try {
        setSnapshot(await invoke<Snapshot>("set_skip_model_switch_confirmation", { skip: true }));
      } catch (error) {
        setMessage(messageFrom(error));
        setIsError(true);
        return;
      }
    }
    void selectAccount(account);
  }

  function cancelAccountSwitch() {
    setAccountSwitchConfirm((prev) => ({ ...prev, open: false }));
  }

  async function addAccount() {
    setBusy("login");
    try {
      const login = await invoke<ProviderLoginLaunch>("launch_provider_login", { provider });
      const providerLabel = PROVIDERS.find((item) => item.id === provider)?.label ?? provider;
      const codeMessage = login.userCode ? ` Enter code ${login.userCode} if asked.` : "";
      try {
        await openUrl(login.authorizationUrl);
        setMessage(`Finish the official ${providerLabel} login in your browser…${codeMessage}`);
        setIsError(false);
      } catch (openError) {
        setMessage(
          `Login started, but the browser did not open automatically (${messageFrom(openError)}). Open this URL manually: ${login.authorizationUrl}.${codeMessage}`,
        );
        setIsError(true);
      }
      await refresh(true);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function cancelLogin() {
    setBusy("cancel-login");
    try {
      setSnapshot(await invoke<Snapshot>("cancel_provider_login"));
      setMessage("Provider login cancelled. Live credentials were not changed.");
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function openDiagnosticsFolder() {
    try {
      await invoke("open_diagnostics_folder");
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    }
  }

  async function openBasiliskosClaude() {
    setBusy("open-claude");
    try {
      setSnapshot(await invoke<Snapshot>("launch_hydra_claude"));
      setMessage("Opened the separate Basiliskos Claude window. Your normal Claude app is untouched.");
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function closeBasiliskosClaude() {
    setBusy("close-claude");
    try {
      setSnapshot(await invoke<Snapshot>("stop_hydra_claude"));
      setMessage("Closed only the Basiliskos Claude window");
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function minimizeWindow() {
    await getCurrentWindow().minimize();
  }

  async function toggleWindowMaximize() {
    await getCurrentWindow().toggleMaximize();
  }

  async function hideWindow() {
    await getCurrentWindow().hide();
  }

  async function downloadUpdate(release: Release) {
    const destination = release.installerUrl ?? release.releaseUrl;
    if (!destination) {
      setMessage(`No Windows installer is attached to ${release.name}.`);
      setIsError(true);
      return;
    }
    setBusy("download-update");
    try {
      await openUrl(destination);
      setMessage(release.installerUrl
        ? `Downloading ${release.name}. Run the downloaded installer to update Basiliskos.`
        : `Opened ${release.name} on GitHub. Download the Windows installer from that release.`);
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  const availableUpdate = releases.find((release) => isNewerVersion(release.tagName, APP_VERSION));

  return (
    <main className="app-shell">
      <header className="topbar" data-tauri-drag-region>
        <div className="brand">
          <img src={brandArt} alt="Basiliskos crowned serpent emblem" />
          <div>
            <h1>Basiliskos</h1>
            <p>Local model relay for Claude Code</p>
          </div>
        </div>
        <div className="topbar-right">
          {availableUpdate && (
            <button className="update-indicator" onClick={() => setView("changes")} title={`${availableUpdate.name} is available`}>
              <BellDot size={15} /> Update {availableUpdate.tagName.replace(/^v/i, "")} available
            </button>
          )}
          <div className="health-indicators" aria-label="Basiliskos health">
            <StatusBadge label="Relay" status={snapshot?.relay} />
            <StatusBadge label="Engine" status={snapshot?.backend} />
          </div>
          <div className="window-controls" aria-label="Window controls">
            <button type="button" aria-label="Minimize Basiliskos" title="Minimize" onClick={() => void minimizeWindow()}><Minus size={15} /></button>
            <button type="button" aria-label="Maximize Basiliskos" title="Maximize" onClick={() => void toggleWindowMaximize()}><Maximize2 size={14} /></button>
            <button type="button" className="close-control" aria-label="Hide Basiliskos to tray" title="Hide to tray" onClick={() => void hideWindow()}><X size={15} /></button>
          </div>
        </div>
      </header>

      <nav className="app-tabs" aria-label="Basiliskos sections">
        <button className={view === "console" ? "selected" : ""} aria-current={view === "console" ? "page" : undefined} onClick={() => setView("console")}>Console</button>
        <button className={view === "changes" ? "selected" : ""} aria-current={view === "changes" ? "page" : undefined} onClick={() => setView("changes")}>Changes{availableUpdate && <i aria-label="Update available" />}</button>
      </nav>

      {view === "console" ? <>
      <section className="hero" aria-label="Current connection">
        <div className="hero-watermark" aria-hidden="true" style={{ backgroundImage: `url(${brandArt})` }} />
        <div className="hero-copy">
          <span className="eyebrow">NOW SERVING · CLAUDE CODE</span>
          <h2>{active && activeRoute ? activeRoute.selectedModelLabel : "No account selected"}</h2>
          <p>
            {active && activeRoute
              ? `${PROVIDERS.find((item) => item.id === active.provider)?.label} · ${active.label} · Thinking ${thinkingLabel(activeRoute.thinking)}${contextWindowLabel(activeRoute.contextWindow) ? ` · ${contextWindowLabel(activeRoute.contextWindow)}` : ""}`
              : "Add an account, then choose Use account."}
          </p>
        </div>
        <div className="hero-actions">
          <span className={`token-status ${active ? "ok" : "muted"}`}>
            <i aria-hidden="true" />{active ? "Credential selected · local" : "No active credential"}
          </span>
          <button className="secondary" onClick={() => void startOrStop()} disabled={busy !== null}>
            {busy === "power" ? <LoaderCircle className="spin" size={17} /> : snapshot?.running ? <CircleStop size={17} /> : <Play size={17} />}
            {snapshot?.running ? "Stop relay" : "Start relay"}
          </button>
        </div>
      </section>

      <div className="choices-grid">
        <section className="panel accounts-panel" aria-label="Choose account">
          <div className="panel-head">
            <div><span className="zone-label">CHOOSE ACCOUNT</span><h2>Authorized subscriptions</h2></div>
            {loginWaiting ? (
              <button className="add-button cancel-login" onClick={() => void cancelLogin()} disabled={busy !== null}>
                {busy === "cancel-login" ? <LoaderCircle className="spin" size={15} /> : <X size={15} />} Cancel login
              </button>
            ) : (
              <button className="add-button" onClick={() => void addAccount()} disabled={busy !== null}>
                {busy === "login" ? <LoaderCircle className="spin" size={15} /> : <LogIn size={15} />} Add account
              </button>
            )}
          </div>
          <div className="provider-tabs" role="tablist" aria-label="Account provider">
            {PROVIDERS.map((item) => (
              <button key={item.id} role="tab" aria-selected={provider === item.id} className={provider === item.id ? "selected" : ""} onClick={() => setProvider(item.id)}>
                {item.label}
              </button>
            ))}
          </div>
          <div className="account-list" role="tabpanel">
            {accounts.length === 0 ? (
              <div className="empty-state"><ShieldCheck size={26} /><h3>No {PROVIDERS.find((item) => item.id === provider)?.label} account yet</h3><p>Add one using the official browser login.</p></div>
            ) : accounts.map((account) => {
              const usage = usageByAccount[account.fileName];
              const isEditing = editingAccount === account.fileName;
              return (
                <article className={`account-row ${account.active ? "active" : ""}`} key={account.fileName}>
                  <div className="account-avatar">{account.label.slice(0, 1).toUpperCase()}</div>
                  <div className="account-copy">
                    {isEditing ? (
                      <form className="account-name-form" onSubmit={(event) => { event.preventDefault(); void renameAccount(account); }}>
                        <label className="sr-only" htmlFor={`profile-name-${account.fileName}`}>Profile name</label>
                        <input
                          id={`profile-name-${account.fileName}`}
                          value={draftName}
                          onChange={(event) => setDraftName(event.target.value)}
                          onKeyDown={(event) => { if (event.key === "Escape") cancelRename(); }}
                          maxLength={64}
                          autoFocus
                        />
                        <button type="submit" className="inline-icon-button save" aria-label={`Save name for ${account.label}`} title="Save name" disabled={busy !== null}><Check size={14} /></button>
                        <button type="button" className="inline-icon-button" aria-label="Cancel rename" title="Cancel" onClick={cancelRename} disabled={busy !== null}><X size={14} /></button>
                      </form>
                    ) : (
                      <div className="account-name-line"><strong>{account.label}</strong>{account.active && <span>Active</span>}</div>
                    )}
                    <p>{account.email ?? "Authorized account"}</p>
                    <div className="usage-summary">
                      {usage?.data ? usage.data.windows.map((window) => (
                        <div className={`usage-window ${window.remainingPercent < 20 ? "low" : ""}`} key={window.label} title={`${Math.round(window.usedPercent)}% used`}>
                          <span>{window.label}</span>
                          <progress max="100" value={window.remainingPercent} aria-label={`${window.label}: ${Math.round(window.remainingPercent)} percent remaining`} />
                          <strong>{Math.round(window.remainingPercent)}% left</strong>
                        </div>
                      )) : usage?.loading ? (
                        <span className="usage-state"><LoaderCircle className="spin" size={11} /> Checking usage…</span>
                      ) : (
                        <span className="usage-state unavailable" title={usage?.error}>
                          {usage?.error ?? "Usage unavailable"}
                        </span>
                      )}
                    </div>
                  </div>
                  <div className="account-actions">
                    {!account.active && <button className="use-button" onClick={() => requestAccountSelection(account)} disabled={busy !== null}>{busy === account.fileName ? <LoaderCircle className="spin" size={15} /> : <Power size={15} />} Use account</button>}
                    {!isEditing && <button className="icon-button" aria-label={`Rename ${account.label}`} title={`Rename ${account.label}`} onClick={() => beginRename(account)} disabled={busy !== null}><Pencil size={15} /></button>}
                    <button className="icon-button danger" aria-label={`Remove ${account.label}`} title={`Remove ${account.label}`} onClick={() => void removeAccount(account)} disabled={busy !== null}><Trash2 size={16} /></button>
                  </div>
                </article>
              );
            })}
          </div>
          <div className="panel-foot account-counts">
            {providerCounts.map((item, index) => (
              <span key={item.id}>{index > 0 && <i aria-hidden="true">·</i>}{item.label} · {item.count}</span>
            ))}
          </div>
        </section>

        <section className="panel route-panel" aria-label="Choose model" aria-busy={busy === "route"}>
          <div className="panel-head"><div><span className="zone-label">CHOOSE MODEL</span><h2>Route for the next request</h2></div></div>
          <div className="route-body">
            <label><span>Model</span><select value={activeRoute?.selectedModel ?? ""} onChange={(event) => chooseModel(event.target.value)} disabled={busy !== null || !activeRoute}>{!activeRoute && <option value="">Choose an account first</option>}{activeRoute?.modelOptions.map((model) => <option value={model.id} key={model.id}>{model.label}</option>)}</select></label>
            <label><span>Thinking</span><select value={activeRoute?.thinking ?? "auto"} onChange={(event) => void updateRoute(activeRoute?.selectedModel ?? "", event.target.value)} disabled={busy !== null || !activeRoute || !selectedModel?.thinkingLevels.length}><option value="auto">Auto</option>{selectedModel?.thinkingLevels.map((level) => <option value={level} key={level}>{thinkingLabel(level)}</option>)}</select></label>
            <p className="route-note">Changes apply to the next request from the Basiliskos Claude window. Thinking levels depend on the selected model.</p>
          </div>
          <div className="panel-foot claude-foot"><ShieldCheck size={16} /><div><strong>Basiliskos Claude window</strong> · <span className={snapshot?.claudeRunning ? "running-dot" : "stopped-dot"}>● {snapshot?.claude.state ?? "unknown"}</span><br />{snapshot?.claude.detail ?? "Waiting for controller status"}</div>{snapshot?.claudeRunning ? <button onClick={() => void closeBasiliskosClaude()} disabled={busy !== null}>Close window</button> : <button onClick={() => void openBasiliskosClaude()} disabled={busy !== null || !snapshot?.activeAccount || snapshot?.backend.state !== "healthy"}><AppWindow size={15} /> Open window</button>}</div>
        </section>
      </div>

      {showDiagnostics && (
        <section className="diagnostics-panel" aria-label="Basiliskos diagnostics">
          <div className="diagnostics-head">
            <div><span className="zone-label">DIAGNOSTICS</span><h2>Redacted controller activity</h2></div>
            <div className="diagnostics-actions">
              <button onClick={() => void refresh()}><RefreshCw size={15} /> Refresh</button>
              <button onClick={() => void openDiagnosticsFolder()}><FolderOpen size={15} /> Open logs</button>
              <button aria-label="Close diagnostics" onClick={() => setShowDiagnostics(false)}><X size={15} /></button>
            </div>
          </div>
          <div className="diagnostics-summary">
            {[snapshot?.controller, snapshot?.relay, snapshot?.backend, snapshot?.credentials, snapshot?.route, snapshot?.oauth, snapshot?.claude].map((status, index) => (
              <div key={index}><span className={statusTone(status)}><i aria-hidden="true" />{status?.state ?? "unknown"}</span><p>{status?.detail ?? "No status available"}</p></div>
            ))}
          </div>
          <div className="event-list">
            <DiagnosticEventList events={snapshot?.diagnostics ?? []} />
          </div>
        </section>
      )}
      </> : (
        <section className="changes-panel" aria-label="Basiliskos updates and changes">
          <div className="changes-head">
            <div><span className="zone-label">UPDATES</span><h2>{availableUpdate ? `${availableUpdate.name} is available` : "Basiliskos is up to date"}</h2><p>Current version {APP_VERSION}</p></div>
            <div className="changes-actions">
              <button onClick={() => void checkForUpdates()} disabled={checkingUpdates || busy !== null}>{checkingUpdates ? <LoaderCircle className="spin" size={15} /> : <RefreshCw size={15} />} Check now</button>
              {availableUpdate && <button className="primary" onClick={() => void downloadUpdate(availableUpdate)} disabled={busy !== null}><Download size={15} /> Download update</button>}
            </div>
          </div>
          {updateError && <p className="update-error">Could not reach the update service: {updateError}</p>}
          <div className="release-list">
            {releases.length === 0 && !checkingUpdates && !updateError && <p className="no-events">No published releases found yet.</p>}
            {releases.map((release) => (
              <article className={`release-entry ${release === availableUpdate ? "available" : ""}`} key={release.tagName}>
                <div className="release-heading"><div><h3>{release.name}</h3><p>{release.tagName} · {release.publishedAt ? new Date(release.publishedAt).toLocaleDateString() : "Published release"}</p></div>{release === availableUpdate && <span>New</span>}</div>
                <p className="release-notes">{release.body}</p>
                {release === availableUpdate && (release.installerUrl || release.releaseUrl) && <button className="download-inline" onClick={() => void downloadUpdate(release)} disabled={busy !== null}><Download size={14} /> {release.installerUrl ? `Download ${release.tagName}` : `View ${release.tagName}`}</button>}
              </article>
            ))}
          </div>
        </section>
      )}

      {accountSwitchConfirm.open && (
        <div className="modal-backdrop" role="presentation" onClick={cancelAccountSwitch}>
          <div className="modal" role="alertdialog" aria-modal="true" aria-labelledby="account-switch-title" onClick={(event) => event.stopPropagation()}>
            <h3 id="account-switch-title">Switch account?</h3>
            <p>This will close and reopen the Basiliskos Claude window. Any in-progress request in that window will be interrupted.</p>
            <label className="modal-checkbox">
              <input
                type="checkbox"
                checked={accountSwitchConfirm.dontShowAgain}
                onChange={(event) => setAccountSwitchConfirm((prev) => ({ ...prev, dontShowAgain: event.target.checked }))}
              />
              <span>Don't show again</span>
            </label>
            <div className="modal-actions">
              <button onClick={cancelAccountSwitch}>Cancel</button>
              <button className="primary" onClick={() => void confirmAccountSwitch()}>Continue</button>
            </div>
          </div>
        </div>
      )}

      <footer><p className={isError ? "error-message" : ""} aria-live="polite" aria-atomic="true">{message} {view === "console" && <button className="activity-link" onClick={() => setShowDiagnostics((current) => !current)}>Activity {showDiagnostics ? "▾" : "▸"}</button>}</p><span>Basiliskos {APP_VERSION} · CLIProxyAPI {snapshot?.version ?? "…"}</span></footer>
    </main>
  );
}
