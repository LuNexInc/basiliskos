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
  Copy,
  Download,
  ExternalLink,
  FolderOpen,
  ListFilter,
  LoaderCircle,
  LogIn,
  Maximize2,
  Minus,
  Pencil,
  Play,
  RefreshCw,
  ShieldCheck,
  Terminal,
  Timer,
  Trash2,
  X,
} from "lucide-react";
import brandArt from "./assets/basiliskos-mark.png";
import "./App.css";

type Provider = "claude" | "codex" | "xai" | "kimi" | "deepseek";

/**
 * DeepSeek is the one provider with no OAuth/device flow — it is authorized with
 * an API key, so the "Add account" button opens a key field instead of starting
 * a browser login.
 */
export function usesApiKeyAuth(provider: Provider): boolean {
  return provider === "deepseek";
}

type Account = {
  fileName: string;
  provider: Provider;
  email?: string;
  label: string;
  disabled: boolean;
  active: boolean;
  cooldownUntilMs?: number;
  expiresAtMs?: number;
  credentialStatus: "active" | "renewal_due" | "relogin_required" | "expired" | "unknown";
};

type UsageWindow = {
  label: string;
  usedPercent: number;
  remainingPercent: number;
  resetsAtMs?: number;
  known: boolean;
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

type AccountSelectionResult = Snapshot & { claudeConfigChanged: boolean };
type DeepseekAccountAdded = Snapshot & { fileName: string };

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

type ModelCatalogEntry = {
  id: string;
  label: string;
  hidden: boolean;
  live: boolean | null;
};

type ActiveServiceIdentities = {
  relayEmail?: string;
  codexCliEmail?: string;
  grokCliEmail?: string;
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

/** In-flight OAuth details held in the UI so the user can copy/open deliberately. */
type PendingAuthLaunch = {
  sessionId: string;
  provider: Provider;
  authorizationUrl: string;
  userCode?: string;
  accountLabel?: string;
};

/**
 * Providers whose browser session cookie often auto-completes the wrong account.
 * For these we never auto-open the default browser — surface the URL + code instead
 * so the user can paste into a private window or dedicated profile.
 */
export function prefersManualAuthBrowser(provider: Provider): boolean {
  return provider === "xai" || provider === "kimi";
}

export async function copyTextToClipboard(text: string): Promise<void> {
  if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(text);
    return;
  }
  // Fallback for non-secure contexts / older WebViews
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.setAttribute("readonly", "");
  textarea.style.position = "fixed";
  textarea.style.left = "-9999px";
  document.body.appendChild(textarea);
  textarea.select();
  const ok = document.execCommand("copy");
  document.body.removeChild(textarea);
  if (!ok) throw new Error("Clipboard copy is unavailable in this environment");
}

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

type PreparedBasiliskosUpdate = {
  token: string;
  tagName: string;
  installerName: string;
};

type AppView = "console" | "changes";

const APP_VERSION = "2.2.7";
const RELEASES_URL = "https://api.github.com/repos/LuNexInc/basiliskos/releases?per_page=12";

const PROVIDERS: Array<{ id: Provider; label: string; detail: string }> = [
  { id: "claude", label: "Claude", detail: "Claude OAuth" },
  { id: "codex", label: "Codex", detail: "ChatGPT / Codex OAuth" },
  { id: "xai", label: "Grok", detail: "Grok Build OAuth" },
  { id: "kimi", label: "Kimi", detail: "Kimi Code OAuth" },
  { id: "deepseek", label: "DeepSeek", detail: "DeepSeek API key" },
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

const THINKING_LEVELS = ["auto", "none", "low", "medium", "high", "xhigh", "max", "ultra"];

function QuotaBar({ segments = 16, percent }: { segments?: number; percent: number }) {
  const lit = Math.max(0, Math.min(segments, Math.round((percent / 100) * segments)));
  return (
    <div className="quota-track" role="img" aria-label={`${Math.round(percent)} percent remaining`}>
      {Array.from({ length: segments }, (_, index) => (
        <span key={index} className={index < lit ? "lit" : ""} />
      ))}
    </div>
  );
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

function cooldownRemaining(cooldownUntilMs: number | undefined, now: number) {
  if (!cooldownUntilMs) return 0;
  return Math.max(0, cooldownUntilMs - now);
}

function cooldownLabel(remainingMs: number) {
  const totalSeconds = Math.ceil(remainingMs / 1000);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

export function credentialAlert(account: Account, now: number) {
  if (account.credentialStatus === "relogin_required") {
    return { label: "Sign in again", tone: "relogin" };
  }
  if (account.credentialStatus === "renewal_due") {
    return { label: "Login refresh needed", tone: "renewal" };
  }
  if (account.credentialStatus === "expired" || (account.expiresAtMs && account.expiresAtMs <= now)) {
    return { label: "Login expired", tone: "expired" };
  }
  return undefined;
}

export function usageResetLabel(resetsAtMs: number | undefined) {
  if (!resetsAtMs) return undefined;
  const reset = new Date(resetsAtMs);
  if (Number.isNaN(reset.getTime())) return undefined;
  return `Renews ${reset.toLocaleString(undefined, { day: "numeric", month: "short", hour: "numeric", minute: "2-digit" })}`;
}

export function accountNeedsRelogin(
  account: Pick<Account, "credentialStatus">,
  usageError?: string,
): boolean {
  return account.credentialStatus === "relogin_required" || usageError?.includes("Re-login once") === true;
}

export function usageAccountFiles(accounts: Array<Pick<Account, "fileName" | "provider">>): string[] {
  return accounts
    .filter((account) => account.provider !== "deepseek")
    .map((account) => account.fileName);
}

function ClaudeCodeMark({ className }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        fillRule="evenodd"
        d="M2,3 H14 V10 H2 Z M0,5 H2 V7 H0 Z M14,5 H16 V7 H14 Z M5,5 H7 V8 H5 Z M9,5 H11 V8 H9 Z M2,10 H4 V13 H2 Z M5,10 H7 V13 H5 Z M9,10 H11 V13 H9 Z M12,10 H14 V13 H12 Z"
      />
    </svg>
  );
}

export default function App() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [provider, setProvider] = useState<Provider>("codex");
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState("Starting Basiliskos…");
  const [isError, setIsError] = useState(false);
  const [usageByAccount, setUsageByAccount] = useState<Record<string, UsageLoadState>>({});
  const [editingAccount, setEditingAccount] = useState<string | null>(null);
  const [draftName, setDraftName] = useState("");
  const [showDiagnostics, setShowDiagnostics] = useState(false);
  const [modelCatalog, setModelCatalog] = useState<ModelCatalogEntry[] | null>(null);
  const [modelCatalogBusy, setModelCatalogBusy] = useState(false);
  const [activeIdentities, setActiveIdentities] = useState<ActiveServiceIdentities | null>(null);
  const [servingBusy, setServingBusy] = useState<string | null>(null);
  const [view, setView] = useState<AppView>("console");
  const [releases, setReleases] = useState<Release[]>([]);
  const [checkingUpdates, setCheckingUpdates] = useState(false);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [preparedUpdate, setPreparedUpdate] = useState<PreparedBasiliskosUpdate | null>(null);
  const handledLogin = useRef<string | null>(null);
  const [pendingAuth, setPendingAuth] = useState<PendingAuthLaunch | null>(null);
  const [authCopyFeedback, setAuthCopyFeedback] = useState<string | null>(null);
  // DeepSeek API-key entry. Held only until the key is handed to the backend.
  const [apiKeyPrompt, setApiKeyPrompt] = useState(false);
  const [apiKeyDraft, setApiKeyDraft] = useState("");
  const [accountSwitchConfirm, setAccountSwitchConfirm] = useState<{
    open: boolean;
    account: Account | null;
    dontShowAgain: boolean;
  }>({ open: false, account: null, dontShowAgain: false });
  const [pendingConfirm, setPendingConfirm] = useState<{ message: string; resolve: (value: boolean) => void } | null>(null);

  const confirmDialog = useCallback((message: string) => {
    return new Promise<boolean>((resolve) => {
      setPendingConfirm({ message, resolve });
    });
  }, []);

  function resolvePendingConfirm(value: boolean) {
    pendingConfirm?.resolve(value);
    setPendingConfirm(null);
  }

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
    const interval = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(interval);
  }, []);

  useEffect(() => {
    const login = snapshot?.login;
    // Drop the manual-auth card when this session leaves "waiting". Do not clear when
    // login is still null right after launch — refresh may not have landed yet.
    if (
      pendingAuth &&
      login &&
      login.sessionId === pendingAuth.sessionId &&
      login.state !== "waiting"
    ) {
      setPendingAuth(null);
      setAuthCopyFeedback(null);
    }
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
        const selected = await invoke<AccountSelectionResult>("select_gateway_account", {
          fileName: login.resultFileName,
        });
        const next = selected.claudeRunning
          ? selected
          : await invoke<Snapshot>("launch_hydra_claude");
        setSnapshot(next);
        setProvider(login.provider);
        setPendingAuth(null);
        setAuthCopyFeedback(null);
        setMessage("Account authorized and selected. The isolated Basiliskos Claude window is ready.");
        setIsError(false);
      } catch (error) {
        setMessage(messageFrom(error));
        setIsError(true);
      } finally {
        setBusy(null);
      }
    })();
  }, [snapshot?.login, pendingAuth]);

  const accounts = useMemo(
    () => snapshot?.accounts.filter((account) => account.provider === provider) ?? [],
    [provider, snapshot],
  );
  const allUsageFiles = usageAccountFiles(snapshot?.accounts ?? []);
  const allUsageKey = snapshot?.accounts
    .filter((account) => account.provider !== "deepseek")
    .map((account) => `${account.fileName}|${account.expiresAtMs ?? ""}|${account.credentialStatus}`)
    .join("\u0000") ?? "";
  const allUsageLoading = allUsageFiles.some((fileName) => usageByAccount[fileName]?.loading === true);
  const active = snapshot?.accounts.find((account) => account.active);
  const activeRoute = snapshot?.routes.find((route) => route.provider === active?.provider);
  const selectedModel = activeRoute?.modelOptions.find(
    (model) => model.id === activeRoute.selectedModel,
  );
  const loginWaiting = snapshot?.login?.state === "waiting";
  const codexCliAccount = snapshot?.accounts.find((account) => !!account.email && account.email === activeIdentities?.codexCliEmail);
  const grokCliAccount = snapshot?.accounts.find((account) => !!account.email && account.email === activeIdentities?.grokCliEmail);
  const providerCounts = PROVIDERS.map((item) => ({
    ...item,
    count: snapshot?.accounts.filter((account) => account.provider === item.id).length ?? 0,
  }));

  const getPrimaryUsagePercent = useCallback((fileName?: string) => {
    if (!fileName) return undefined;
    const windows = usageByAccount[fileName]?.data?.windows;
    if (!windows || windows.length === 0) return undefined;
    const knownWindows = windows.filter((w) => w.known);
    if (knownWindows.length === 0) return undefined;
    return knownWindows[0].remainingPercent;
  }, [usageByAccount]);

  const activeUsagePercent = getPrimaryUsagePercent(active?.fileName);
  const codexUsagePercent = getPrimaryUsagePercent(codexCliAccount?.fileName);
  const grokUsagePercent = getPrimaryUsagePercent(grokCliAccount?.fileName);

  useEffect(() => {
    const unlistenPromise = getCurrentWindow().listen("tauri://focus", () => {
      setView("console");
    });
    return () => {
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);
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
    void refreshUsage(allUsageFiles);
    const interval = window.setInterval(() => void refreshUsage(allUsageFiles), 5 * 60_000);
    return () => window.clearInterval(interval);
  }, [allUsageKey, refreshUsage]);

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
    const wasRunning = snapshot?.claudeRunning === true;
    setBusy(account.fileName);
    try {
      const result = await invoke<AccountSelectionResult>("select_gateway_account", {
        fileName: account.fileName,
      });
      let next: Snapshot = result;
      if (wasRunning) {
        if (result.claudeConfigChanged) {
          await invoke<Snapshot>("stop_hydra_claude");
          next = await invoke<Snapshot>("launch_hydra_claude");
        }
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
    if (!(await confirmDialog(`Remove ${account.label} from Basiliskos?`))) return;
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

  async function openModelCatalog() {
    if (!active) return;
    setModelCatalogBusy(true);
    try {
      const entries = await invoke<ModelCatalogEntry[]>("get_model_catalog", { provider: active.provider });
      setModelCatalog(entries);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setModelCatalogBusy(false);
    }
  }

  function closeModelCatalog() {
    setModelCatalog(null);
  }

  async function toggleModelHidden(entry: ModelCatalogEntry) {
    setModelCatalogBusy(true);
    try {
      const nextHidden = !entry.hidden;
      const next = await invoke<Snapshot>("set_model_hidden", { modelId: entry.id, hidden: nextHidden });
      setSnapshot(next);
      setModelCatalog((current) =>
        current?.map((item) => (item.id === entry.id ? { ...item, hidden: nextHidden } : item)) ?? current,
      );
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setModelCatalogBusy(false);
    }
  }

  const loadActiveIdentities = useCallback(async () => {
    try {
      setActiveIdentities(await invoke<ActiveServiceIdentities>("active_service_identities"));
    } catch {
      // Best-effort display signal only — never surface this as an error.
    }
  }, []);

  useEffect(() => {
    void loadActiveIdentities();
    const interval = window.setInterval(() => void loadActiveIdentities(), 5000);
    return () => window.clearInterval(interval);
  }, [loadActiveIdentities]);

  async function serveCodexCliFromRelay(account: Account) {
    if (!(await confirmDialog(`Make "${account.label}" serve the real Codex CLI too (~/.codex/auth.json)? Any tool reading that file live — the Codex Desktop app, background bots — is affected too.`))) {
      return;
    }
    setServingBusy(account.fileName);
    try {
      await invoke("serve_codex_cli_from_relay", { relayFileName: account.fileName, closeRunning: false });
      await loadActiveIdentities();
      setMessage(`Real Codex CLI is now using "${account.label}".`);
      setIsError(false);
    } catch (error) {
      const detail = messageFrom(error);
      if (detail.includes("Close the running Codex CLI") && (await confirmDialog(`${detail}. Close it and switch anyway?`))) {
        try {
          await invoke("serve_codex_cli_from_relay", { relayFileName: account.fileName, closeRunning: true });
          await loadActiveIdentities();
          setMessage(`Real Codex CLI is now using "${account.label}".`);
          setIsError(false);
        } catch (retryError) {
          setMessage(messageFrom(retryError));
          setIsError(true);
        }
      } else {
        setMessage(detail);
        setIsError(true);
      }
    } finally {
      setServingBusy(null);
    }
  }

  async function serveGrokCliFromRelay(account: Account) {
    if (!(await confirmDialog(`Make "${account.label}" serve the real Grok CLI too (~/.grok/auth.json)? Any tool reading that file live — background bots included — is affected too.`))) {
      return;
    }
    setServingBusy(account.fileName);
    try {
      await invoke("serve_grok_cli_from_relay", { relayFileName: account.fileName, closeRunning: false });
      await loadActiveIdentities();
      setMessage(`Real Grok CLI is now using "${account.label}".`);
      setIsError(false);
    } catch (error) {
      const detail = messageFrom(error);
      if (detail.includes("Close the running Grok CLI") && (await confirmDialog(`${detail}. Close it and switch anyway?`))) {
        try {
          await invoke("serve_grok_cli_from_relay", { relayFileName: account.fileName, closeRunning: true });
          await loadActiveIdentities();
          setMessage(`Real Grok CLI is now using "${account.label}".`);
          setIsError(false);
        } catch (retryError) {
          setMessage(messageFrom(retryError));
          setIsError(true);
        }
      } else {
        setMessage(detail);
        setIsError(true);
      }
    } finally {
      setServingBusy(null);
    }
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

  async function presentProviderLogin(
    login: ProviderLoginLaunch,
    loginProvider: Provider,
    options?: { accountLabel?: string },
  ) {
    const providerLabel = PROVIDERS.find((item) => item.id === loginProvider)?.label ?? loginProvider;
    const codeMessage = login.userCode ? ` Enter code ${login.userCode} if asked.` : "";
    const pending: PendingAuthLaunch = {
      sessionId: login.sessionId,
      provider: loginProvider,
      authorizationUrl: login.authorizationUrl,
      userCode: login.userCode,
      accountLabel: options?.accountLabel,
    };
    setPendingAuth(pending);
    setAuthCopyFeedback(null);

    // Manual path: never hand the URL to the default browser (cookie jar often
    // auto-completes the wrong account when adding a second Grok/Kimi credential).
    if (prefersManualAuthBrowser(loginProvider)) {
      const target = options?.accountLabel ? ` for "${options.accountLabel}"` : "";
      setMessage(
        `Finish the official ${providerLabel} login${target} in a private window or dedicated browser profile — use Copy URL below.${codeMessage}`,
      );
      setIsError(false);
      return;
    }

    try {
      await openUrl(login.authorizationUrl);
      const target = options?.accountLabel ? ` to refresh "${options.accountLabel}"` : "";
      setMessage(`Finish the official ${providerLabel} login in your browser${target}…${codeMessage}`);
      setIsError(false);
    } catch (openError) {
      setMessage(
        `Login started, but the browser did not open automatically (${messageFrom(openError)}). Use Copy URL below, or open this URL manually: ${login.authorizationUrl}.${codeMessage}`,
      );
      setIsError(true);
    }
  }

  async function addAccount() {
    // API-key providers have no browser login to launch — collect the key first.
    if (usesApiKeyAuth(provider)) {
      setApiKeyDraft("");
      setApiKeyPrompt(true);
      setMessage("Paste a DeepSeek API key from platform.deepseek.com to authorize it.");
      setIsError(false);
      return;
    }
    setBusy("login");
    try {
      const login = await invoke<ProviderLoginLaunch>("launch_provider_login", { provider });
      await presentProviderLogin(login, provider);
      await refresh(true);
    } catch (error) {
      setPendingAuth(null);
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function submitApiKey() {
    const key = apiKeyDraft.trim();
    if (!key) {
      setMessage("Enter a DeepSeek API key first.");
      setIsError(true);
      return;
    }
    setBusy("api-key");
    try {
      // The backend verifies the key against DeepSeek before saving it, so a
      // bad key fails here rather than during a later relay request.
      const added = await invoke<DeepseekAccountAdded>("add_deepseek_account", { apiKey: key });
      setSnapshot(added);
      setApiKeyPrompt(false);
      setApiKeyDraft("");
      setIsError(false);
      // A newly added account is stored disabled, so adding alone would not
      // change what Claude is talking to. Switch to it immediately through the
      // normal selection path, which also restarts the Basiliskos Claude window
      // when its configuration changes.
      const account = added.accounts.find((item) => item.fileName === added.fileName);
      if (account) {
        await selectAccount(account);
      } else {
        setMessage("DeepSeek API key saved. Select the account to route through it.");
        await refresh(true);
      }
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  function cancelApiKeyPrompt() {
    setApiKeyPrompt(false);
    setApiKeyDraft("");
  }

  async function relogin(account: Account) {
    // Re-authorizing an API-key account means replacing the key, not re-running
    // a login the provider does not have.
    if (usesApiKeyAuth(account.provider)) {
      setProvider(account.provider);
      setApiKeyDraft("");
      setApiKeyPrompt(true);
      setMessage(`Paste a new DeepSeek API key to replace the one on "${account.label}".`);
      setIsError(false);
      return;
    }
    setBusy(`relogin-${account.fileName}`);
    try {
      const login = await invoke<ProviderLoginLaunch>("launch_provider_login", { provider: account.provider });
      await presentProviderLogin(login, account.provider, { accountLabel: account.label });
      await refresh(true);
    } catch (error) {
      setPendingAuth(null);
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function copyAuthField(kind: "url" | "code") {
    if (!pendingAuth) return;
    const value = kind === "url" ? pendingAuth.authorizationUrl : pendingAuth.userCode;
    if (!value) return;
    try {
      await copyTextToClipboard(value);
      setAuthCopyFeedback(kind === "url" ? "URL copied" : "Code copied");
      setIsError(false);
    } catch (error) {
      setAuthCopyFeedback(null);
      setMessage(`Could not copy to clipboard: ${messageFrom(error)}`);
      setIsError(true);
    }
  }

  async function openAuthUrlManually() {
    if (!pendingAuth) return;
    try {
      await openUrl(pendingAuth.authorizationUrl);
      setMessage("Opened the authorization URL in your default browser. Prefer a private window if the wrong account is signed in.");
      setIsError(false);
    } catch (error) {
      setMessage(`Could not open the browser (${messageFrom(error)}). Use Copy URL and paste it yourself.`);
      setIsError(true);
    }
  }

  async function cancelLogin() {
    setBusy("cancel-login");
    try {
      setSnapshot(await invoke<Snapshot>("cancel_provider_login"));
      setPendingAuth(null);
      setAuthCopyFeedback(null);
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
    setBusy("download-update");
    try {
      const prepared = await invoke<PreparedBasiliskosUpdate>("prepare_basiliskos_update", { tagName: release.tagName });
      setPreparedUpdate(prepared);
      setMessage(`${prepared.tagName} was downloaded and its SHA-256 checksum was verified.`);
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function confirmUpdateInstall() {
    if (!preparedUpdate) return;
    setBusy("install-update");
    try {
      await invoke("install_basiliskos_update", { token: preparedUpdate.token });
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
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
            <StatusBadge label="Local server" status={snapshot?.relay} />
            <StatusBadge label="Provider link" status={snapshot?.backend} />
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
        <div className="hero-services">
          <div className="hero-service">
            <span className="eyebrow">Claude Code</span>
            <h3>{active && activeRoute ? activeRoute.selectedModelLabel : "No account"}</h3>
            <p>
              {active && activeRoute
                ? `${active.label} · Thinking ${thinkingLabel(activeRoute.thinking)}${contextWindowLabel(activeRoute.contextWindow) ? ` · ${contextWindowLabel(activeRoute.contextWindow)}` : ""}`
                : "Serve an account below"}
            </p>
            {activeUsagePercent !== undefined ? (
              <div className="hero-fuel">
                <QuotaBar percent={activeUsagePercent} />
                <span>{Math.round(activeUsagePercent)}% fuel left</span>
              </div>
            ) : (
              <div className="hero-fuel unrecorded">
                <span>Usage unrecorded</span>
              </div>
            )}
          </div>
          <div className="hero-service">
            <span className="eyebrow">Codex CLI</span>
            <h3>{codexCliAccount ? codexCliAccount.label : "Not set"}</h3>
            <p>{codexCliAccount ? codexCliAccount.email ?? "Real codex command" : "Serve an account below"}</p>
            {codexUsagePercent !== undefined ? (
              <div className="hero-fuel">
                <QuotaBar percent={codexUsagePercent} />
                <span>{Math.round(codexUsagePercent)}% fuel left</span>
              </div>
            ) : (
              <div className="hero-fuel unrecorded">
                <span>Usage unrecorded</span>
              </div>
            )}
          </div>
          <div className="hero-service">
            <span className="eyebrow">Grok CLI</span>
            <h3>{grokCliAccount ? grokCliAccount.label : "Not set"}</h3>
            <p>{grokCliAccount ? grokCliAccount.email ?? "Real grok command" : "Serve an account below"}</p>
            {grokUsagePercent !== undefined ? (
              <div className="hero-fuel">
                <QuotaBar percent={grokUsagePercent} />
                <span>{Math.round(grokUsagePercent)}% fuel left</span>
              </div>
            ) : (
              <div className="hero-fuel unrecorded">
                <span>Usage unrecorded</span>
              </div>
            )}
          </div>
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
            <div style={{ display: "flex", gap: 8 }}>
              <button
                className="add-button"
                onClick={() => void refreshUsage(allUsageFiles)}
                disabled={allUsageFiles.length === 0 || allUsageLoading}
                title="Refresh usage for every account now. Basiliskos also refreshes automatically every five minutes."
              >
                <RefreshCw className={allUsageLoading ? "spin" : undefined} size={15} /> Refresh usage
              </button>
              {loginWaiting ? (
                <button className="add-button cancel-login" onClick={() => void cancelLogin()} disabled={busy !== null}>
                  {busy === "cancel-login" ? <LoaderCircle className="spin" size={15} /> : <X size={15} />} Cancel login
                </button>
              ) : (
                <button className="add-button" onClick={() => void addAccount()} disabled={busy !== null}>
                  {busy === "login" ? <LoaderCircle className="spin" size={15} /> : <LogIn size={15} />}{" "}
                  {usesApiKeyAuth(provider) ? "Add API key" : "Add account"}
                </button>
              )}
            </div>
          </div>
          <div className="provider-tabs" role="tablist" aria-label="Account provider">
            {PROVIDERS.map((item) => (
              <button key={item.id} role="tab" aria-selected={provider === item.id} className={provider === item.id ? "selected" : ""} onClick={() => { setProvider(item.id); cancelApiKeyPrompt(); }}>
                {item.label}
              </button>
            ))}
          </div>
          {apiKeyPrompt && (
            <div className="auth-wait-card" role="group" aria-label="DeepSeek API key">
              <div className="auth-wait-head">
                <span className="zone-label">API KEY</span>
                <strong>DeepSeek</strong>
                <p>
                  DeepSeek authorizes with an API key instead of a browser login. Create one at
                  platform.deepseek.com, then paste it here. It is stored locally and verified with
                  DeepSeek before it is saved.
                </p>
              </div>
              <div className="auth-wait-url-row">
                <input
                  className="auth-wait-url"
                  type="password"
                  autoComplete="off"
                  spellCheck={false}
                  aria-label="DeepSeek API key"
                  placeholder="sk-…"
                  value={apiKeyDraft}
                  onChange={(event) => setApiKeyDraft(event.target.value)}
                  onKeyDown={(event) => { if (event.key === "Enter") void submitApiKey(); }}
                  disabled={busy !== null}
                />
                <button type="button" className="auth-wait-action" onClick={() => void submitApiKey()} disabled={busy !== null || !apiKeyDraft.trim()}>
                  {busy === "api-key" ? <LoaderCircle className="spin" size={14} /> : <ShieldCheck size={14} aria-hidden="true" />} Verify and save
                </button>
                <button type="button" className="auth-wait-action" onClick={cancelApiKeyPrompt} disabled={busy !== null}>
                  <X size={14} aria-hidden="true" /> Cancel
                </button>
              </div>
            </div>
          )}
          {pendingAuth && (
            <div className="auth-wait-card" role="status" aria-live="polite">
              <div className="auth-wait-head">
                <span className="zone-label">AUTHORIZATION</span>
                <strong>
                  {PROVIDERS.find((item) => item.id === pendingAuth.provider)?.label ?? pendingAuth.provider}
                  {pendingAuth.accountLabel ? ` · ${pendingAuth.accountLabel}` : ""}
                </strong>
                <p>
                  {prefersManualAuthBrowser(pendingAuth.provider)
                    ? "Open this URL in a private window or a dedicated browser profile so the right account is chosen. The default browser often auto-completes the wrong one."
                    : "Finish the official provider login in your browser. You can also copy the URL if auto-open failed."}
                </p>
              </div>
              <div className="auth-wait-url-row">
                <code className="auth-wait-url" title={pendingAuth.authorizationUrl}>{pendingAuth.authorizationUrl}</code>
                <button type="button" className="auth-wait-action" onClick={() => void copyAuthField("url")} disabled={busy !== null}>
                  <Copy size={14} aria-hidden="true" /> Copy URL
                </button>
                <button type="button" className="auth-wait-action" onClick={() => void openAuthUrlManually()} disabled={busy !== null}>
                  <ExternalLink size={14} aria-hidden="true" /> Open browser
                </button>
              </div>
              {pendingAuth.userCode && (
                <div className="auth-wait-code-row">
                  <span className="auth-wait-code-label">Code</span>
                  <code className="auth-wait-code">{pendingAuth.userCode}</code>
                  <button type="button" className="auth-wait-action" onClick={() => void copyAuthField("code")} disabled={busy !== null}>
                    <Copy size={14} aria-hidden="true" /> Copy code
                  </button>
                </div>
              )}
              {authCopyFeedback && <p className="auth-wait-feedback">{authCopyFeedback}</p>}
            </div>
          )}
          <div className="account-list" role="tabpanel">
            {accounts.length === 0 ? (
              <div className="empty-state"><ShieldCheck size={26} /><h3>No {PROVIDERS.find((item) => item.id === provider)?.label} account yet</h3><p>Add one using the official browser login.</p></div>
            ) : accounts.map((account) => {
              const usage = usageByAccount[account.fileName];
              const isEditing = editingAccount === account.fileName;
              const cooling = cooldownRemaining(account.cooldownUntilMs, now);
              const credentialWarning = credentialAlert(account, now);
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
                      <div className="account-name-line">
                        <strong>{account.label}</strong>
                        {cooling > 0 && (
                          <span className="cooldown-chip" title="Rate-limited by the provider; cools down automatically">
                            <Timer size={11} /> {cooldownLabel(cooling)}
                          </span>
                        )}
                      </div>
                    )}
                    <p>{account.email ?? "Authorized account"}</p>
                    {credentialWarning && (
                      <div className={`credential-expiry ${credentialWarning.tone}`}>
                        <Timer size={11} aria-hidden="true" /> {credentialWarning.label}
                      </div>
                    )}
                    <div className="usage-summary">
                      {usage?.data ? usage.data.windows.map((window) => window.known ? (
                        <div className={`usage-window ${window.remainingPercent < 20 ? "low" : ""}`} key={window.label} title={`${Math.round(window.usedPercent)}% used`}>
                          <span>{window.label}</span>
                          <QuotaBar percent={window.remainingPercent} />
                          <strong>{Math.round(window.remainingPercent)}% left</strong>
                          {usageResetLabel(window.resetsAtMs) && <small>{usageResetLabel(window.resetsAtMs)}</small>}
                        </div>
                      ) : (
                        <div className="usage-window unrecorded" key={window.label} title="The provider returned a billing period but did not report a usage percentage.">
                          <span>{window.label}</span>
                          <span className="usage-unrecorded">Not reported</span>
                          {usageResetLabel(window.resetsAtMs) && <small>{usageResetLabel(window.resetsAtMs)}</small>}
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
                    {accountNeedsRelogin(account, usage?.error) && (
                      <button
                        className="icon-button warn"
                        aria-label={`Re-login ${account.label}`}
                        onClick={() => void relogin(account)}
                        disabled={busy !== null}
                        title="Token expired or rejected — click to sign in again"
                      >
                        {busy === `relogin-${account.fileName}` ? <LoaderCircle className="spin" size={15} /> : <LogIn size={15} />}
                      </button>
                    )}
                    <button
                      className={`icon-button serve-toggle ${account.active ? "active" : ""}`}
                      aria-label={account.active ? `${account.label} is serving Claude Code` : `Serve Claude Code with ${account.label}`}
                      onClick={() => requestAccountSelection(account)}
                      disabled={busy !== null || cooling > 0 || account.active}
                    >
                      <span className="serve-toggle-fill">
                        <span className="serve-toggle-icon">
                          {busy === account.fileName ? <LoaderCircle className="spin" size={15} /> : <ClaudeCodeMark className="claude-mark-icon" />}
                        </span>
                        <span className="serve-toggle-label">
                          {account.active ? "Serving Claude Code" : cooling > 0 ? `Cooling down ${cooldownLabel(cooling)}` : "Use for Claude Code"}
                        </span>
                      </span>
                    </button>
                    {account.provider === "codex" && (
                      <button
                        className={`icon-button serve-toggle cli ${account.email === activeIdentities?.codexCliEmail ? "active" : ""}`}
                        aria-label={account.email === activeIdentities?.codexCliEmail ? `${account.label} is serving the real Codex CLI` : `Serve real Codex CLI with ${account.label}`}
                        onClick={() => void serveCodexCliFromRelay(account)}
                        disabled={servingBusy !== null || account.email === activeIdentities?.codexCliEmail}
                      >
                        <span className="serve-toggle-fill">
                          <span className="serve-toggle-icon">
                            {servingBusy === account.fileName ? <LoaderCircle className="spin cli-icon" size={15} /> : <Terminal className="cli-icon" size={15} />}
                          </span>
                          <span className="serve-toggle-label">
                            {account.email === activeIdentities?.codexCliEmail ? "Serving Codex CLI" : "Use for Codex CLI"}
                          </span>
                        </span>
                      </button>
                    )}
                    {account.provider === "xai" && (
                      <button
                        className={`icon-button serve-toggle cli ${account.email === activeIdentities?.grokCliEmail ? "active" : ""}`}
                        aria-label={account.email === activeIdentities?.grokCliEmail ? `${account.label} is serving the real Grok CLI` : `Serve real Grok CLI with ${account.label}`}
                        onClick={() => void serveGrokCliFromRelay(account)}
                        disabled={servingBusy !== null || account.email === activeIdentities?.grokCliEmail}
                      >
                        <span className="serve-toggle-fill">
                          <span className="serve-toggle-icon">
                            {servingBusy === account.fileName ? <LoaderCircle className="spin cli-icon" size={15} /> : <Terminal className="cli-icon" size={15} />}
                          </span>
                          <span className="serve-toggle-label">
                            {account.email === activeIdentities?.grokCliEmail ? "Serving Grok CLI" : "Use for Grok CLI"}
                          </span>
                        </span>
                      </button>
                    )}
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
            <div className="chip-field">
              <div className="chip-field-head">
                <span>Model</span>
                {activeRoute && (
                  <button
                    type="button"
                    className="manage-models-button"
                    onClick={() => void openModelCatalog()}
                    disabled={busy !== null}
                    aria-label="Manage which models show up here"
                    title="Manage which models show up here"
                  >
                    <ListFilter size={12} /> Manage
                  </button>
                )}
              </div>
              {activeRoute ? (
                <div className="chip-row" role="radiogroup" aria-label="Model">
                  {activeRoute.modelOptions.map((model) => (
                    <button
                      type="button"
                      key={model.id}
                      role="radio"
                      aria-checked={activeRoute.selectedModel === model.id}
                      className={`chip ${activeRoute.selectedModel === model.id ? "selected" : ""}`}
                      onClick={() => chooseModel(model.id)}
                      disabled={busy !== null}
                    >
                      {model.label}
                    </button>
                  ))}
                </div>
              ) : (
                <p className="chip-empty">Choose an account first</p>
              )}
            </div>
            <div className="chip-field">
              <span>Thinking</span>
              <div className="chip-row" role="radiogroup" aria-label="Thinking">
                {THINKING_LEVELS.map((level) => {
                  const supported = level === "auto" || (selectedModel?.thinkingLevels.includes(level) ?? false);
                  const checked = (activeRoute?.thinking ?? "auto") === level;
                  return (
                    <button
                      type="button"
                      key={level}
                      role="radio"
                      aria-checked={checked}
                      className={`chip ${checked ? "selected" : ""}`}
                      onClick={() => void updateRoute(activeRoute?.selectedModel ?? "", level)}
                      disabled={busy !== null || !activeRoute || !supported}
                      title={supported ? undefined : `${selectedModel?.label ?? "This model"} doesn't support ${thinkingLabel(level)} thinking`}
                    >
                      {thinkingLabel(level)}
                    </button>
                  );
                })}
              </div>
            </div>
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
              {availableUpdate && <button className="primary" onClick={() => void downloadUpdate(availableUpdate)} disabled={busy !== null}><Download size={15} /> Install update</button>}
            </div>
          </div>
          {updateError && <p className="update-error">Could not reach the update service: {updateError}</p>}
          <div className="release-list">
            {releases.length === 0 && !checkingUpdates && !updateError && <p className="no-events">No published releases found yet.</p>}
            {releases.map((release) => (
              <article className={`release-entry ${release === availableUpdate ? "available" : ""}`} key={release.tagName}>
                <div className="release-heading"><div><h3>{release.name}</h3><p>{release.tagName} · {release.publishedAt ? new Date(release.publishedAt).toLocaleDateString() : "Published release"}</p></div>{release === availableUpdate && <span>New</span>}</div>
                <p className="release-notes">{release.body}</p>
                {release === availableUpdate && <button className="download-inline" onClick={() => void downloadUpdate(release)} disabled={busy !== null}><Download size={14} /> Install ${release.tagName}</button>}
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

      {pendingConfirm && (
        <div className="modal-backdrop" role="presentation" onClick={() => resolvePendingConfirm(false)}>
          <div className="modal" role="alertdialog" aria-modal="true" aria-labelledby="pending-confirm-title" onClick={(event) => event.stopPropagation()}>
            <h3 id="pending-confirm-title">Basiliskos</h3>
            <p>{pendingConfirm.message}</p>
            <div className="modal-actions">
              <button onClick={() => resolvePendingConfirm(false)}>Cancel</button>
              <button className="primary" onClick={() => resolvePendingConfirm(true)}>Continue</button>
            </div>
          </div>
        </div>
      )}

      {preparedUpdate && (
        <div className="modal-backdrop" role="presentation" onClick={() => setPreparedUpdate(null)}>
          <div className="modal" role="alertdialog" aria-modal="true" aria-labelledby="update-install-title" onClick={(event) => event.stopPropagation()}>
            <h3 id="update-install-title">Install {preparedUpdate.tagName}?</h3>
            <p>{preparedUpdate.installerName} was downloaded and its SHA-256 checksum matched the published release manifest. Basiliskos will close, then Windows will ask for administrator approval and open the installer. Basiliskos itself does not stay elevated.</p>
            <div className="modal-actions">
              <button onClick={() => setPreparedUpdate(null)} disabled={busy === "install-update"}>Cancel</button>
              <button className="primary" onClick={() => void confirmUpdateInstall()} disabled={busy === "install-update"}>{busy === "install-update" ? "Launching…" : "Install and close"}</button>
            </div>
          </div>
        </div>
      )}

      {modelCatalog && (
        <div className="modal-backdrop" role="presentation" onClick={closeModelCatalog}>
          <div className="modal model-catalog-modal" role="dialog" aria-modal="true" aria-labelledby="model-catalog-title" onClick={(event) => event.stopPropagation()}>
            <h3 id="model-catalog-title">Manage models</h3>
            <p>Hide models you don't want cluttering the list. Once Basiliskos has checked the backend for this provider, models it doesn't report as available are flagged automatically.</p>
            <div className="model-catalog-list">
              {modelCatalog.map((entry) => (
                <label key={entry.id} className="model-catalog-row">
                  <input type="checkbox" checked={!entry.hidden} onChange={() => void toggleModelHidden(entry)} disabled={modelCatalogBusy} />
                  <span className="model-catalog-name">{entry.label}</span>
                  <span
                    className={`model-catalog-live ${entry.live === true ? "live" : entry.live === false ? "stale" : "unknown"}`}
                    title={entry.live === true ? "The backend reports this model as available" : entry.live === false ? "The backend did not report this model as available" : "Not checked yet — this updates once an account of this provider is active"}
                  >
                    <i aria-hidden="true" />{entry.live === true ? "Live" : entry.live === false ? "Not seen" : "Unchecked"}
                  </span>
                </label>
              ))}
            </div>
            <div className="modal-actions">
              <button onClick={closeModelCatalog}>Done</button>
            </div>
          </div>
        </div>
      )}

      <footer><p className={isError ? "error-message" : ""} aria-live="polite" aria-atomic="true">{message} {view === "console" && <button className="activity-link" onClick={() => setShowDiagnostics((current) => !current)}>Activity {showDiagnostics ? "▾" : "▸"}</button>}</p><span>Basiliskos {APP_VERSION} · CLIProxyAPI {snapshot?.version ?? "…"}</span></footer>
    </main>
  );
}
