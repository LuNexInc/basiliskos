import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  contextWindowLabel,
  messageFrom,
  statusTone,
  thinkingLabel,
} from "./ui";
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
  KeyRound,
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

type Provider =
  | "claude"
  | "codex"
  | "xai"
  | "kimi"
  | "antigravity"
  | "deepseek"
  | "opencode"
  | "openrouter"
  | "litellm"
  | "custom";

const API_KEY_PROVIDERS: Provider[] = ["deepseek", "opencode", "openrouter", "litellm", "custom"];

function isApiKeyProvider(provider: Provider): boolean {
  return API_KEY_PROVIDERS.includes(provider);
}

function providerAuthKind(provider: Provider): "oauth" | "api_key" {
  return isApiKeyProvider(provider) ? "api_key" : "oauth";
}

type Account = {
  fileName: string;
  provider: Provider;
  email?: string;
  label: string;
  disabled: boolean;
  active: boolean;
  activeForCodex: boolean;
  cooldownUntilMs?: number;
  expiresAtMs?: number;
  auth?: "oauth" | "api_key";
  baseUrl?: string;
  credentialStatus: "active" | "renewal_due" | "relogin_required" | "expired" | "unknown" | "configured" | "disabled";
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
  codexRunning: boolean;
  accounts: Account[];
  activeAccount?: string;
  routes: ProviderRoute[];
  activeCodexAccount?: string;
  codexRoutes: ProviderRoute[];
  autoFailover?: { fromLabel: string; toLabel: string; atMs: number };
  controller: ComponentStatus;
  relay: ComponentStatus;
  backend: ComponentStatus;
  credentials: ComponentStatus;
  route: ComponentStatus;
  oauth: ComponentStatus;
  claude: ComponentStatus;
  codex: ComponentStatus;
  backendExitReason?: string;
  activeRequests: number;
  diagnostics: DiagnosticEvent[];
  login?: ProviderLoginStatus;
  skipModelSwitchConfirmation: boolean;
  openClaudeOnLaunch: boolean;
};

type AccountSelectionResult = Snapshot & { claudeConfigChanged: boolean };
type RouteUpdateResult = Snapshot & { routeVerified: boolean };

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
  name: string;
  body: string;
  publishedAt: string;
  releaseUrl: string;
  installerUrl?: string;
};

type PreparedBasiliskosUpdate = {
  token: string;
  tagName: string;
  installerName: string;
};

type AppView = "console" | "changes";
type ProviderFilter = "all" | Provider;

export const APP_VERSION = "3.0.0";

const PROVIDERS: Array<{ id: Provider; label: string; detail: string; group: "oauth" | "api-key" }> = [
  { id: "claude", label: "Claude", detail: "Claude OAuth", group: "oauth" },
  { id: "codex", label: "Codex", detail: "ChatGPT / Codex OAuth", group: "oauth" },
  { id: "xai", label: "Grok", detail: "Grok Build OAuth", group: "oauth" },
  { id: "kimi", label: "Kimi", detail: "Kimi Code OAuth / Moonshot key", group: "oauth" },
  { id: "antigravity", label: "Antigravity", detail: "Google Antigravity OAuth", group: "oauth" },
  { id: "deepseek", label: "DeepSeek", detail: "DeepSeek API key", group: "api-key" },
  { id: "opencode", label: "OpenCode Go", detail: "OpenCode API key", group: "api-key" },
  { id: "openrouter", label: "OpenRouter", detail: "OpenRouter API key", group: "api-key" },
  { id: "litellm", label: "LiteLLM", detail: "Self-hosted proxy", group: "api-key" },
  { id: "custom", label: "Custom", detail: "Any OpenAI-compatible endpoint", group: "api-key" },
];

const PROVIDER_NAMES: Record<Provider, string> = {
  claude: "Claude",
  codex: "Codex",
  xai: "Grok",
  kimi: "Kimi",
  antigravity: "Antigravity",
  deepseek: "DeepSeek",
  opencode: "OpenCode Go",
  openrouter: "OpenRouter",
  litellm: "LiteLLM",
  custom: "Custom",
};

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

function HeroFuel({ percent }: { percent?: number }) {
  if (percent === undefined) {
    return (
      <div className="hero-fuel unrecorded">
        <span>Usage unrecorded</span>
      </div>
    );
  }
  return (
    <div className="hero-fuel">
      <QuotaBar segments={8} percent={percent} />
      <span>{Math.round(percent)}% left</span>
    </div>
  );
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

export { statusTone } from "./ui";

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

export function usageAccountFiles(accounts: Array<Pick<Account, "fileName" | "provider" | "auth">>): string[] {
  return accounts
    .filter((account) => account.provider !== "antigravity")
    .filter((account) => account.auth !== "api_key")
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

/// The official Codex wordmark (zonalogo.com colored variant), inlined so the
/// isolated Codex affordance carries the real brand. Each instance gets a
/// unique gradient id so multiple buttons on one page never share a def.
function CodexMark({ className }: { className?: string }) {
  const uid = useId().replace(/[^a-zA-Z0-9]/g, "");
  const grad = `codex-g-${uid}`;
  return (
    <svg className={className} viewBox="0 0 250 250" aria-hidden="true" focusable="false">
      <defs>
        <linearGradient id={grad} x2="1" gradientTransform="matrix(0,249.335,-249.128,0,125,.332)">
          <stop stopColor="#b1a7ff" />
          <stop offset=".5" stopColor="#7a9dff" />
          <stop offset="1" stopColor="#3941ff" />
        </linearGradient>
      </defs>
      <path
        fill={`url(#${grad})`}
        d="m84.3 5.1q3.7-1.5 7.7-2.6 3.9-1 7.9-1.6 4-0.5 8.1-0.6 4 0 8 0.5 20.7 2.4 37.1 17.7 0.1 0.1 0.4 0.3 0.1 0 0.2 0 0 0 0.2 0 0 0 0.1 0 0 0 0.1 0 5.2-1.4 10.7-1.9 5.4-0.4 10.7 0.1 5.5 0.4 10.7 1.9 5.2 1.3 10.1 3.6l0.6 0.4 1.6 0.8q5.2 2.5 9.7 6.1 4.7 3.4 8.6 7.7 3.8 4.3 6.9 9.2 3 4.8 5.2 10.2 4.3 10.5 4.3 22.1 0.2 2.1 0 4.2-0.1 2.2-0.2 4.3-0.3 2.1-0.7 4.3-0.4 2.1-0.9 4.1 0 0.2 0 0.4 0 0.2 0 0.5 0 0.1 0.1 0.4 0.1 0.1 0.3 0.3 12.3 12.6 16.3 30 6 29.7-12.2 53.5l-1.9 2.2q-3 3.5-6.5 6.4-3.4 3.1-7.3 5.5-3.8 2.4-8.1 4.2-4.1 1.9-8.5 3.2-0.3 0-0.4 0.2-0.3 0-0.4 0.1-0.1 0.1-0.3 0.4 0 0.1-0.1 0.3c-2.7 7.7-5.3 14.2-10.2 20.7-12.5 16.5-30.8 25.5-51.5 25.5q-24.6-0.1-43.6-18.1-0.2-0.1-0.4-0.2-0.2-0.1-0.4-0.1-0.2 0-0.3 0-0.3 0-0.4 0c-5.4 1.7-10.9 1.9-16.7 1.9q-3.5 0-7-0.5-3.4-0.4-6.9-1.2-3.3-0.8-6.6-2-3.3-1.2-6.4-2.8-3.3-1.6-6.4-3.6-3-2-5.8-4.3-3-2.3-5.5-5-2.5-2.6-4.6-5.6c-2.2-2.7-4.3-5.4-5.8-8.5q-0.8-1.6-1.6-3.2-0.6-1.7-1.3-3.3-0.7-1.7-1.2-3.4-0.5-1.6-1-3.4-1.1-4-1.6-7.9-0.6-4-0.6-8 0-4 0.6-8 0.4-4 1.4-8 0 0 0-0.1 0-0.1 0-0.1 0.2-0.2 0.2-0.3 0-0.1-0.2-0.1 0-0.2 0-0.3 0-0.1-0.1-0.1 0-0.2 0-0.2-0.1-0.1-0.1-0.1-2.4-2.5-4.6-5.2-2.1-2.7-4-5.4-1.7-3-3.2-6-1.5-3.1-2.6-6.3-0.8-2-1.3-4.1-0.7-2-1.1-4-0.4-2.1-0.7-4.2-0.2-2.2-0.4-4.3-0.2-2.8-0.1-5.6 0-2.8 0.3-5.4 0.1-2.8 0.6-5.6 0.4-2.8 1.1-5.5 7-23.1 26.9-36.3 4.3-2.9 8.2-4.5 4.5-1.9 9-3.2 0.2 0 0.3-0.1 0.1-0.2 0.3-0.3 0.1 0 0.1-0.3 0.1-0.1 0.1-0.2 1-3.1 2.2-6 1-2.9 2.5-5.7 1.5-3 3.2-5.6 1.7-2.7 3.7-5.1 2.5-3.2 5.3-5.9 3-2.8 6.1-5.4 3.2-2.4 6.8-4.4 3.5-2 7.2-3.5zm48.3 146.4c-2.3 0.1-4.4 1-6 2.8-1.5 1.6-2.4 3.7-2.4 5.9 0 2.3 0.9 4.4 2.4 6.2 1.6 1.6 3.7 2.5 6 2.6h50.4c2.4 0.1 4.8-0.6 6.5-2.4 1.7-1.6 2.8-4 2.8-6.4 0-2.4-1.1-4.7-2.8-6.3-1.7-1.8-4.1-2.6-6.5-2.4zm-56.7-64.9c-1.2-1.9-3-3.4-5.3-3.9-2.2-0.5-4.5-0.3-6.5 0.9-2 1.1-3.5 3-4.1 5.2-0.7 2.2-0.4 4.6 0.6 6.5l17.7 30.9-17.5 29.5c-1.2 2-1.6 4.5-1.1 6.8 0.7 2.3 2.1 4.1 4.1 5.3 2 1.2 4.4 1.6 6.7 0.9 2.2-0.5 4.2-1.9 5.4-3.9l20.1-34.1q0.7-0.9 0.9-2.1 0.3-1.1 0.3-2.3 0-1.2-0.3-2.2-0.2-1.2-0.8-2.2z"
      />
    </svg>
  );
}

export default function App() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [provider, setProvider] = useState<Provider>("codex");
  const [providerFilter, setProviderFilter] = useState<ProviderFilter>("all");
  const [apiKeyForm, setApiKeyForm] = useState<null | { key: string; baseUrl: string; model: string; label: string }>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState("Starting BasiliskOS…");
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
  const lastFailoverAtMs = useRef<number | null>(null);
  const [pendingAuth, setPendingAuth] = useState<PendingAuthLaunch | null>(null);
  const [authCopyFeedback, setAuthCopyFeedback] = useState<string | null>(null);
  const [accountSwitchConfirm, setAccountSwitchConfirm] = useState<{
    open: boolean;
    account: Account | null;
    dontShowAgain: boolean;
    surface: "claude" | "codex";
  }>({ open: false, account: null, dontShowAgain: false, surface: "claude" });
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
      // Routed through the backend command: server-side fetch (no CORS, no
      // webview rate-limit exposure) with the full release metadata attached.
      const release = await invoke<LatestPublishedRelease>("latest_basiliskos_release");
      const next: Release[] = [{
        tagName: release.tagName,
        name: release.name || `BasiliskOS ${release.tagName}`,
        publishedAt: release.publishedAt,
        body: release.body || "Release details are available on GitHub.",
        releaseUrl: release.releaseUrl,
        installerUrl: release.installerUrl,
      }];
      setReleases(next);
      setUpdateError(null);
      const latest = next.find((release) => isNewerVersion(release.tagName, APP_VERSION));
      if (latest && !quiet) {
        setMessage(`${latest.name} is ready to download.`);
        setIsError(false);
      } else if (!quiet) {
        setMessage("BasiliskOS is up to date.");
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
        if (next.activeAccount && next.openClaudeOnLaunch !== false) {
          const launched = await invoke<Snapshot>("launch_hydra_claude");
          setSnapshot(launched);
          setMessage("Relay ready. Opened the separate BasiliskOS Claude window.");
        } else {
          setSnapshot(next);
          setMessage(
            next.activeAccount
              ? "Relay ready. Choose Open BasiliskOS Claude when you want the window."
              : "Relay ready. Add or choose an account.",
          );
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
    const failover = snapshot?.autoFailover;
    if (!failover) return;
    if (lastFailoverAtMs.current === failover.atMs) return;
    lastFailoverAtMs.current = failover.atMs;
    setMessage(
      `${failover.fromLabel} was rate-limited; BasiliskOS switched to ${failover.toLabel}.`,
    );
    setIsError(false);
  }, [snapshot?.autoFailover]);

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
        setPendingAuth(null);
        setAuthCopyFeedback(null);
        const activeAccount = snapshot?.accounts.find((account) => account.active);
        if (activeAccount) {
          // Signing in or renewing a login must not disturb the active route.
          // The credential is already committed; the user chooses when to
          // switch with "Use account".
          setProvider(login.provider);
          setMessage(
            "Account authorized. Choose Use account to route the BasiliskOS window to it.",
          );
          setIsError(false);
        } else {
          // First account on this machine: make it active and open the window.
          const selected = await invoke<AccountSelectionResult>("select_gateway_account", {
            fileName: login.resultFileName,
            client: "claude",
          });
          const next = selected.claudeRunning
            ? selected
            : await invoke<Snapshot>("launch_hydra_claude");
          setSnapshot(next);
          setProvider(login.provider);
          setMessage("Account authorized and selected. The isolated BasiliskOS Claude window is ready.");
          setIsError(false);
        }
      } catch (error) {
        setMessage(messageFrom(error));
        setIsError(true);
      } finally {
        setBusy(null);
      }
    })();
  }, [snapshot?.login, pendingAuth]);

  const accounts = useMemo(
    () => providerFilter === "all"
      ? snapshot?.accounts ?? []
      : snapshot?.accounts.filter((account) => account.provider === providerFilter) ?? [],
    [providerFilter, snapshot],
  );
  const totalAccountCount = snapshot?.accounts.length ?? 0;
  const allUsageFiles = usageAccountFiles(snapshot?.accounts ?? []);
  const allUsageKey = snapshot?.accounts
    .filter((account) => account.provider !== "antigravity")
    .map((account) => `${account.fileName}|${account.expiresAtMs ?? ""}|${account.credentialStatus}`)
    .join("\u0000") ?? "";
  const allUsageLoading = allUsageFiles.some((fileName) => usageByAccount[fileName]?.loading === true);
  const active = snapshot?.accounts.find((account) => account.active);
  const activeRoute = snapshot?.routes.find((route) => route.provider === active?.provider);
  const activeCodex = snapshot?.accounts.find((account) => account.activeForCodex);
  const codexActiveRoute = snapshot?.codexRoutes.find((route) => route.provider === activeCodex?.provider);
  const selectedModel = activeRoute?.modelOptions.find(
    (model) => model.id === activeRoute.selectedModel,
  );
  const loginWaiting = snapshot?.login?.state === "waiting";
  const codexCliAccount = snapshot?.accounts.find((account) => account.provider === "codex" && !!account.email && account.email === activeIdentities?.codexCliEmail);
  const grokCliAccount = snapshot?.accounts.find((account) => account.provider === "xai" && !!account.email && account.email === activeIdentities?.grokCliEmail);
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
        client: "claude",
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
      setMessage(`${account.label} is now serving the separate BasiliskOS Claude window`);
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  /// "Use for BasiliskOS Codex": select the account AND ensure the isolated
  /// Codex window runs on it (relaunch if already open so the new route
  /// applies). Mirrors `selectAccount` for the Codex surface.
  async function useAccountForCodex(account: Account) {
    const wasRunning = snapshot?.codexRunning === true;
    setBusy(`codex-${account.fileName}`);
    try {
      const result = await invoke<AccountSelectionResult>("select_gateway_account", {
        fileName: account.fileName,
        client: "codex",
      });
      let next: Snapshot = result;
      if (wasRunning) {
        await invoke<Snapshot>("stop_hydra_codex_app");
        next = await invoke<Snapshot>("launch_hydra_codex_app");
      } else if (!next.codexRunning) {
        next = await invoke<Snapshot>("launch_hydra_codex_app");
      }
      setSnapshot(next);
      setMessage(`${account.label} is now serving the separate BasiliskOS Codex window`);
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function removeAccount(account: Account) {
    if (!(await confirmDialog(`Remove ${account.label} from BasiliskOS?`))) return;
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
    setMessage("Updating the BasiliskOS route…");
    setIsError(false);
    try {
      const next = await invoke<RouteUpdateResult>("set_gateway_route", {
        provider: active.provider,
        model,
        thinking,
        client: "claude",
      });
      setSnapshot(next);
      const route = next.routes.find((item) => item.provider === active.provider);
      setMessage(
        next.routeVerified
          ? (route
              ? `BasiliskOS now routes to ${route.selectedModelLabel} · ${thinkingLabel(route.thinking)}. Applies to the next request.`
              : "BasiliskOS route updated")
          : "Route saved, but the backend was unreachable so it could not be verified. It will be checked on the next request.",
      );
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function updateCodexRoute(model: string, thinking: string) {
    if (!activeCodex) return;
    setBusy("codex-route");
    try {
      const next = await invoke<RouteUpdateResult>("set_gateway_route", {
        provider: activeCodex.provider,
        model,
        thinking,
        client: "codex",
      });
      setSnapshot(next);
      setMessage("ChatGPT route saved. Restart the isolated ChatGPT window to apply it.");
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
      setAccountSwitchConfirm({ open: true, account, dontShowAgain: false, surface: "claude" });
      return;
    }
    void selectAccount(account);
  }

  function requestCodexAccountSelection(account: Account) {
    if (snapshot?.codexRunning && !snapshot.skipModelSwitchConfirmation) {
      setAccountSwitchConfirm({ open: true, account, dontShowAgain: false, surface: "codex" });
      return;
    }
    void useAccountForCodex(account);
  }

  async function confirmAccountSwitch() {
    const { account, dontShowAgain, surface } = accountSwitchConfirm;
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
    if (surface === "codex") {
      void useAccountForCodex(account);
    } else {
      void selectAccount(account);
    }
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
    if (providerAuthKind(provider) === "api_key") {
      setApiKeyForm({ key: "", baseUrl: "", model: "", label: "" });
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

  async function submitApiKeyAccount() {
    if (!apiKeyForm) return;
    setBusy("login");
    try {
      await invoke<Snapshot>("add_api_key_account", {
        provider,
        label: apiKeyForm.label || `${PROVIDER_NAMES[provider]} account`,
        apiKey: apiKeyForm.key,
        baseUrl: apiKeyForm.baseUrl || null,
        model: apiKeyForm.model || null,
      });
      setApiKeyForm(null);
      await refresh(true);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  function cancelApiKeyForm() {
    setApiKeyForm(null);
  }

  async function showApiKeyModels(account: Account) {
    try {
      const models = await invoke<string[]>("get_api_key_account_models", { fileName: account.fileName });
      setMessage(models.length
        ? `${account.label} models: ${models.join(", ")}`
        : `${account.label}: no models detected yet. Select the account to fetch its live catalog.`);
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    }
  }

  async function relogin(account: Account) {
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

  async function copyDiagnostics() {
    if (!snapshot) return;
    const zones = ["controller", "relay", "backend", "credentials", "route", "oauth", "claude"] as const;
    const lines = [
      `BasiliskOS ${APP_VERSION} · ${new Date().toISOString()}`,
      "",
      ...zones.map((zone) => {
        const status = snapshot[zone];
        return `${zone}: ${status?.state ?? "unknown"} — ${status?.detail ?? ""}`;
      }),
    ];
    if (snapshot.activeAccount) lines.push(`activeAccount: ${snapshot.activeAccount}`);
    lines.push("");
    for (const event of snapshot.diagnostics ?? []) {
      lines.push(
        `[${event.code} ${event.severity}] ${event.timestamp}`
          + (event.httpStatus ? ` HTTP ${event.httpStatus}` : "")
          + (event.provider ? ` (${event.provider})` : "")
          + ` ${event.message}`,
      );
    }
    try {
      await copyTextToClipboard(lines.join("\n"));
      setMessage("Diagnostics copied to clipboard");
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    }
  }

  async function setOpenClaudeOnLaunch(open: boolean) {
    setBusy("settings");
    try {
      setSnapshot(await invoke<Snapshot>("set_open_claude_on_launch", { open }));
      setMessage(open ? "BasiliskOS will reopen the Claude window at launch" : "BasiliskOS will not auto-open the Claude window at launch");
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function openBasiliskosClaude() {
    setBusy("open-claude");
    try {
      setSnapshot(await invoke<Snapshot>("launch_hydra_claude"));
      setMessage("Opened the separate BasiliskOS Claude window. Your normal Claude app is untouched.");
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
      setMessage("Closed only the BasiliskOS Claude window");
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function openBasiliskosCodex() {
    setBusy("open-codex");
    try {
      setSnapshot(await invoke<Snapshot>("launch_hydra_codex_app"));
      setMessage("Opened the separate BasiliskOS Codex window. Your normal Codex app is untouched.");
      setIsError(false);
    } catch (error) {
      setMessage(messageFrom(error));
      setIsError(true);
    } finally {
      setBusy(null);
    }
  }

  async function closeBasiliskosCodex() {
    setBusy("close-codex");
    try {
      setSnapshot(await invoke<Snapshot>("stop_hydra_codex_app"));
      setMessage("Closed only the BasiliskOS Codex window");
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
      <div className="app-chrome">
        <header className="topbar" data-tauri-drag-region>
          <div className="brand">
            <img src={brandArt} alt="BasiliskOS crowned serpent emblem" />
            <div>
              <h1>BasiliskOS</h1>
              <p>Local model relay for Claude Code</p>
            </div>
          </div>
          <div className="topbar-right">
            {availableUpdate && (
              <button className="update-indicator" onClick={() => setView("changes")} title={`${availableUpdate.name} is available`}>
                <BellDot size={15} /> Update {availableUpdate.tagName.replace(/^v/i, "")} available
              </button>
            )}
            <div className="health-indicators" aria-label="BasiliskOS health">
              <StatusBadge label="Local server" status={snapshot?.relay} />
              <StatusBadge label="Provider link" status={snapshot?.backend} />
            </div>
            <div className="window-controls" aria-label="Window controls">
              <button type="button" aria-label="Minimize BasiliskOS" title="Minimize" onClick={() => void minimizeWindow()}><Minus size={15} /></button>
              <button type="button" aria-label="Maximize BasiliskOS" title="Maximize" onClick={() => void toggleWindowMaximize()}><Maximize2 size={14} /></button>
              <button type="button" className="close-control" aria-label="Hide BasiliskOS to tray" title="Hide to tray" onClick={() => void hideWindow()}><X size={15} /></button>
            </div>
          </div>
        </header>

        <nav className="app-tabs" aria-label="BasiliskOS sections">
          <button className={view === "console" ? "selected" : ""} aria-current={view === "console" ? "page" : undefined} onClick={() => setView("console")}>Console</button>
          <button className={view === "changes" ? "selected" : ""} aria-current={view === "changes" ? "page" : undefined} onClick={() => setView("changes")}>Changes{availableUpdate && <i aria-label="Update available" />}</button>
        </nav>

        {view === "console" && (
          <section className="target-matrix" aria-label="Target Engines">
            <div className="target-card">
              <div className="target-card-header">
                <span className="eyebrow">Claude Code</span>
                <span className={`target-status-dot ${snapshot?.claudeRunning ? "running" : "stopped"}`}>
                  ● {snapshot?.claudeRunning ? "Running" : "Stopped"}
                </span>
              </div>
              <div className="target-card-body">
                <h3 title={active && activeRoute ? activeRoute.selectedModelLabel : "No account"}>
                  {active && activeRoute ? activeRoute.selectedModelLabel : "No account"}
                </h3>
                <p>
                  {active && activeRoute
                    ? `${active.label} · Thinking ${thinkingLabel(activeRoute.thinking)}${contextWindowLabel(activeRoute.contextWindow) ? ` · ${contextWindowLabel(activeRoute.contextWindow)}` : ""}`
                    : "Choose an account below"}
                </p>
                <HeroFuel percent={activeUsagePercent} />
              </div>
              <div className="target-card-actions">
                {snapshot?.claudeRunning ? (
                  <button className="target-btn close" onClick={() => void closeBasiliskosClaude()} disabled={busy !== null}>
                    Close window
                  </button>
                ) : (
                  <button
                    className="target-btn open"
                    onClick={() => void openBasiliskosClaude()}
                    disabled={busy !== null || !snapshot?.activeAccount || snapshot?.backend.state !== "healthy"}
                  >
                    <AppWindow size={13} /> Launch window
                  </button>
                )}
              </div>
            </div>

            <div className="target-card">
              <div className="target-card-header">
                <span className="eyebrow">ChatGPT App</span>
                <span className={`target-status-dot ${snapshot?.codexRunning ? "running" : "stopped"}`}>
                  ● {snapshot?.codexRunning ? "Running" : "Stopped"}
                </span>
              </div>
              <div className="target-card-body">
                <h3 title={activeCodex ? activeCodex.label : "No account"}>
                  {activeCodex ? activeCodex.label : "No account"}
                </h3>
                <p>
                  {snapshot?.codexRunning
                    ? `Active on ${activeCodex ? activeCodex.label : "account"} · Effort ${(codexActiveRoute?.thinking ?? "auto") === "auto" ? "Auto" : codexActiveRoute?.thinking}`
                    : "Isolated ChatGPT window"}
                </p>
                <HeroFuel percent={getPrimaryUsagePercent(activeCodex?.fileName)} />
              </div>
              <div className="target-card-actions">
                {snapshot?.codexRunning ? (
                  <button className="target-btn close" onClick={() => void closeBasiliskosCodex()} disabled={busy !== null}>
                    Close window
                  </button>
                ) : (
                  <button
                    className="target-btn open"
                    onClick={() => void openBasiliskosCodex()}
                    disabled={busy !== null || !snapshot?.activeCodexAccount || snapshot?.backend.state !== "healthy"}
                  >
                    <AppWindow size={13} /> Launch window
                  </button>
                )}
              </div>
            </div>

            <div className="target-card">
              <div className="target-card-header">
                <span className="eyebrow">CLI Bridges</span>
                <span className={`target-status-dot ${codexCliAccount || grokCliAccount ? "running" : "stopped"}`}>
                  ● {codexCliAccount || grokCliAccount ? "Configured" : "Idle"}
                </span>
              </div>
              <div className="target-card-body">
                <h3 title={codexCliAccount ? codexCliAccount.label : "Codex: Not set"}>
                  {codexCliAccount ? `Codex: ${codexCliAccount.label}` : "Codex: Not set"}
                </h3>
                <p title={grokCliAccount ? grokCliAccount.label : "Grok: Not set"}>
                  {grokCliAccount ? `Grok: ${grokCliAccount.label}` : "Grok: Not set"}
                </p>
                {(codexCliAccount || grokCliAccount) ? (
                  <div className="cli-fuels">
                    {codexCliAccount && (
                      <div className="cli-fuel">
                        <span className="cli-fuel-label">Codex CLI</span>
                        <HeroFuel percent={codexUsagePercent} />
                      </div>
                    )}
                    {grokCliAccount && (
                      <div className="cli-fuel">
                        <span className="cli-fuel-label">Grok CLI</span>
                        <HeroFuel percent={grokUsagePercent} />
                      </div>
                    )}
                  </div>
                ) : (
                  <span className="usage-state unavailable">No CLI credential selected</span>
                )}
              </div>
              <div className="target-card-actions">
                <span className="cli-hint"><Terminal size={12} /> Terminal active</span>
              </div>
            </div>

            <div className="target-card relay-card">
              <div className="target-card-header">
                <span className="eyebrow">Local Relay</span>
                <span className={`target-status-dot ${snapshot?.running ? "running" : "stopped"}`}>
                  ● {snapshot?.running ? "127.0.0.1:8317" : "Stopped"}
                </span>
              </div>
              <div className="target-card-body">
                <span className={`token-status ${active ? "ok" : "muted"}`}>
                  <i aria-hidden="true" />{active ? "Credential active" : "No active credential"}
                </span>
                <p>{snapshot?.activeRequests ? `${snapshot.activeRequests} active request${snapshot.activeRequests === 1 ? "" : "s"}` : "Relay ready"}</p>
              </div>
              <div className="target-card-actions">
                <button className={`relay-toggle-btn ${snapshot?.running ? "running" : ""}`} onClick={() => void startOrStop()} disabled={busy !== null}>
                  {busy === "power" ? <LoaderCircle className="spin" size={14} /> : snapshot?.running ? <CircleStop size={14} /> : <Play size={14} />}
                  {snapshot?.running ? "Stop relay" : "Start relay"}
                </button>
              </div>
            </div>
          </section>
        )}
      </div>

      {view === "console" ? (
        <>
          <div className="workspace">
            <div className="choices-grid">
              <section className="panel accounts-panel" aria-label="Choose account">
                <div className="panel-head">
                  <div>
                    <span className="zone-label">SUBSCRIPTION FLEET</span>
                    <h2>Authorized accounts</h2>
                  </div>
                  <div style={{ display: "flex", gap: 8 }}>
                    <button
                      className="add-button"
                      onClick={() => void refreshUsage(allUsageFiles)}
                      disabled={allUsageFiles.length === 0 || allUsageLoading}
                      title="Refresh usage for every account now. BasiliskOS also refreshes automatically every five minutes."
                    >
                      <RefreshCw className={allUsageLoading ? "spin" : undefined} size={15} /> Refresh usage
                    </button>
                    {loginWaiting ? (
                      <button className="add-button cancel-login" onClick={() => void cancelLogin()} disabled={busy !== null}>
                        {busy === "cancel-login" ? <LoaderCircle className="spin" size={15} /> : <X size={15} />} Cancel login
                      </button>
                    ) : (
                      <button
                        className="add-button"
                        onClick={() => void addAccount()}
                        disabled={busy !== null || providerFilter === "all"}
                        title={providerFilter === "all"
                          ? "Pick a provider tab above first, then add an account or API key for it."
                          : undefined}
                      >
                        {busy === "login" ? <LoaderCircle className="spin" size={15} /> : <LogIn size={15} />}{" "}
                        "Add account"
                      </button>
                    )}
                  </div>
                </div>
                {apiKeyForm && (
                  <div className="api-key-form" role="group" aria-label={`Add ${PROVIDER_NAMES[provider]} API key`}>
                    <span className="zone-label">API KEY</span>
                    <label className="field-label" htmlFor="api-key-label">Label</label>
                    <input
                      id="api-key-label"
                      className="text-field"
                      value={apiKeyForm.label}
                      onChange={(event) => setApiKeyForm({ ...apiKeyForm, label: event.target.value })}
                      placeholder={`${PROVIDER_NAMES[provider]} account`}
                      maxLength={64}
                    />
                    <label className="field-label" htmlFor="api-key-value">API key</label>
                    <input
                      id="api-key-value"
                      className="text-field"
                      type="password"
                      value={apiKeyForm.key}
                      onChange={(event) => setApiKeyForm({ ...apiKeyForm, key: event.target.value })}
                      placeholder="sk-…"
                    />
                    <label className="field-label" htmlFor="api-key-base-url">Base URL (optional)</label>
                    <input
                      id="api-key-base-url"
                      className="text-field"
                      value={apiKeyForm.baseUrl}
                      onChange={(event) => setApiKeyForm({ ...apiKeyForm, baseUrl: event.target.value })}
                      placeholder="https://…"
                    />
                    <label className="field-label" htmlFor="api-key-model">Model (optional)</label>
                    <input
                      id="api-key-model"
                      className="text-field"
                      value={apiKeyForm.model}
                      onChange={(event) => setApiKeyForm({ ...apiKeyForm, model: event.target.value })}
                      placeholder="deepseek-chat"
                    />
                    <div className="api-key-form-actions">
                      <button className="add-button cancel-login" onClick={cancelApiKeyForm} disabled={busy !== null}>
                        <X size={15} /> Cancel
                      </button>
                      <button
                        className="add-button"
                        onClick={() => void submitApiKeyAccount()}
                        disabled={busy !== null || !apiKeyForm.key.trim()}
                      >
                        {busy === "login" ? <LoaderCircle className="spin" size={15} /> : <LogIn size={15} />} Save API key
                      </button>
                    </div>
                  </div>
                )}
                <div className="provider-tabs" role="tablist" aria-label="Account provider">
                  <button
                    role="tab"
                    aria-selected={providerFilter === "all"}
                    className={providerFilter === "all" ? "selected" : ""}
                    onClick={() => setProviderFilter("all")}
                  >
                    All ({totalAccountCount})
                  </button>
                  {PROVIDERS.map((item) => {
                    const count = snapshot?.accounts.filter((account) => account.provider === item.id).length ?? 0;
                    return (
                      <button
                        key={item.id}
                        role="tab"
                        aria-selected={providerFilter === item.id}
                        className={providerFilter === item.id ? "selected" : ""}
                        onClick={() => {
                          setProviderFilter(item.id);
                          setProvider(item.id);
                        }}
                      >
                        {item.label} ({count})
                      </button>
                    );
                  })}
                </div>
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
                    <div className="empty-state">
                      <ShieldCheck size={26} />
                      {providerFilter === "all" ? (
                        <>
                          <h3>No accounts yet</h3>
                          <p>Pick a provider tab above to add your first account.</p>
                        </>
                      ) : (
                        <>
                          <h3>No {PROVIDERS.find((item) => item.id === providerFilter)?.label ?? "provider"} accounts yet</h3>
                          <p>Add one using the official provider login or API key.</p>
                        </>
                      )}
                    </div>
                  ) : accounts.map((account) => {
                    const usage = usageByAccount[account.fileName];
                    const isEditing = editingAccount === account.fileName;
                    const cooling = cooldownRemaining(account.cooldownUntilMs, now);
                    const credentialWarning = credentialAlert(account, now);
                    return (
                          <article className={`account-row ${account.active || account.activeForCodex ? "active" : ""}`} key={account.fileName}>
                        <div className="account-avatar-wrapper">
                          <div className="account-avatar">{account.label.slice(0, 1).toUpperCase()}</div>
                          <span className={`provider-mini-badge ${account.provider}`}>
                            {PROVIDER_NAMES[account.provider]}
                          </span>
                        </div>
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
                          <p>
                            {account.auth === "api_key"
                              ? account.baseUrl ?? "Custom API key"
                              : account.email ?? "Authorized subscription"}
                          </p>
                          {account.auth === "api_key" && (
                            <div className="credential-expiry key">
                              <KeyRound size={11} aria-hidden="true" /> API key
                            </div>
                          )}
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
                            ) : account.auth === "api_key" ? (
                              <span className="usage-state unrecorded" title="Key-based account; usage is billed by the provider.">
                                Key-based · provider-managed
                              </span>
                            ) : account.provider === "antigravity" ? (
                              <span className="usage-state unrecorded" title="Google Cloud / Gemini quota is managed in the Google Cloud or AI Studio console.">
                                Managed in Google Cloud
                              </span>
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
                          <button
                            className={`icon-button serve-toggle codex ${account.activeForCodex ? "active" : ""}`}
                            aria-label={account.activeForCodex ? `${account.label} is serving BasiliskOS Codex` : `Serve BasiliskOS Codex with ${account.label}`}
                            onClick={() => requestCodexAccountSelection(account)}
                            disabled={busy !== null || cooling > 0 || account.activeForCodex}
                          >
                            <span className="serve-toggle-fill">
                              <span className="serve-toggle-icon">
                                {busy === `codex-${account.fileName}` ? <LoaderCircle className="spin" size={15} /> : <CodexMark className="codex-mark-icon" />}
                              </span>
                              <span className="serve-toggle-label">
                                {account.activeForCodex ? "Serving BasiliskOS Codex" : cooling > 0 ? `Cooling down ${cooldownLabel(cooling)}` : "Use for BasiliskOS Codex"}
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
                          {account.auth === "api_key" && (
                            <button className="icon-button" aria-label={`List models for ${account.label}`} title="List models from this API key" onClick={() => void showApiKeyModels(account)} disabled={busy !== null}>
                              <Terminal size={15} />
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
                <div className="panel-head">
                  <div>
                    <span className="zone-label">ROUTE & PARAMETERS</span>
                    <h2>Next request route</h2>
                  </div>
                </div>
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
                    <span>Thinking / Reasoning Effort</span>
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
                  <div className="chip-field codex-route-field">
                    <div className="chip-field-head"><span>ChatGPT route</span><small>{activeCodex ? activeCodex.label : "Choose a Codex account"}</small></div>
                    {codexActiveRoute ? (
                      <>
                        <div className="chip-row" role="radiogroup" aria-label="ChatGPT model">
                          {codexActiveRoute.modelOptions.map((model) => (
                            <button
                              type="button"
                              key={model.id}
                              role="radio"
                              aria-checked={codexActiveRoute.selectedModel === model.id}
                              className={`chip ${codexActiveRoute.selectedModel === model.id ? "selected" : ""}`}
                              onClick={() => void updateCodexRoute(model.id, codexActiveRoute.thinking)}
                              disabled={busy !== null}
                            >
                              {model.label}
                            </button>
                          ))}
                        </div>
                        <div className="chip-row" role="radiogroup" aria-label="ChatGPT reasoning">
                          {THINKING_LEVELS.map((level) => {
                            const supported = level === "auto" || codexActiveRoute.modelOptions.find((item) => item.id === codexActiveRoute.selectedModel)?.thinkingLevels.includes(level);
                            const checked = codexActiveRoute.thinking === level;
                            return (
                              <button
                                type="button"
                                key={level}
                                role="radio"
                                aria-checked={checked}
                                className={`chip ${checked ? "selected" : ""}`}
                                onClick={() => void updateCodexRoute(codexActiveRoute.selectedModel, level)}
                                disabled={busy !== null || !supported}
                              >
                                {thinkingLabel(level)}
                              </button>
                            );
                          })}
                        </div>
                      </>
                    ) : <p className="chip-empty">Choose a Codex account first</p>}
                  </div>
                </div>
                <div className="window-strip" aria-label="Isolated windows">
                  <div className="window-strip-row">
                    <ShieldCheck size={14} aria-hidden="true" />
                    <span className="window-strip-name">Claude</span>
                    <span className={snapshot?.claudeRunning ? "running-dot" : "stopped-dot"}>
                      {snapshot?.claudeRunning ? "Running" : "Stopped"}
                    </span>
                    {snapshot?.claudeRunning ? (
                      <button type="button" onClick={() => void closeBasiliskosClaude()} disabled={busy !== null}>Close</button>
                    ) : (
                      <button type="button" onClick={() => void openBasiliskosClaude()} disabled={busy !== null || !snapshot?.activeAccount || snapshot?.backend.state !== "healthy"}>
                        <AppWindow size={13} /> Open
                      </button>
                    )}
                  </div>
                  <div className="window-strip-row">
                    <AppWindow size={14} aria-hidden="true" />
                    <span className="window-strip-name">ChatGPT</span>
                    <span className={snapshot?.codexRunning ? "running-dot" : "stopped-dot"}>
                      {snapshot?.codexRunning ? "Running" : "Stopped"}
                    </span>
                    <span
                      className="window-strip-meta"
                      title={activeCodex
                        ? `${activeCodex.label} · ${codexActiveRoute?.selectedModelLabel ?? "—"} · restart ChatGPT after a route change`
                        : "Choose an account first"}
                    >
                      {activeCodex ? (codexActiveRoute?.selectedModelLabel ?? activeCodex.label) : "No account"}
                    </span>
                    {snapshot?.codexRunning ? (
                      <button type="button" onClick={() => void closeBasiliskosCodex()} disabled={busy !== null}>Close</button>
                    ) : (
                      <button type="button" onClick={() => void openBasiliskosCodex()} disabled={busy !== null || !snapshot?.activeCodexAccount || snapshot?.backend.state !== "healthy"}>
                        <AppWindow size={13} /> Open
                      </button>
                    )}
                  </div>
                  <label className="settings-row">
                    <input type="checkbox" checked={snapshot?.openClaudeOnLaunch !== false} onChange={(event) => void setOpenClaudeOnLaunch(event.target.checked)} disabled={busy !== null} />
                    <span>Open Claude at launch</span>
                  </label>
                </div>
              </section>
            </div>
          </div>

          {showDiagnostics && (
            <section className="diagnostics-panel" aria-label="BasiliskOS diagnostics">
              <div className="diagnostics-head">
                <div><span className="zone-label">DIAGNOSTICS</span><h2>Redacted controller activity</h2></div>
                <div className="diagnostics-actions">
                  <button onClick={() => void refresh()}><RefreshCw size={15} /> Refresh</button>
                  <button onClick={() => void openDiagnosticsFolder()}><FolderOpen size={15} /> Open logs</button>
                  <button onClick={() => void copyDiagnostics()}><Copy size={15} /> Copy</button>
                  <button aria-label="Close diagnostics" onClick={() => setShowDiagnostics(false)}><X size={15} /></button>
                </div>
              </div>
              <div className="diagnostics-summary">
                {[snapshot?.controller, snapshot?.relay, snapshot?.backend, snapshot?.credentials, snapshot?.route, snapshot?.oauth, snapshot?.claude, snapshot?.codex].map((status, index) => (
                  <div key={index}><span className={statusTone(status)}><i aria-hidden="true" />{status?.state ?? "unknown"}</span><p>{status?.detail ?? "No status available"}</p></div>
                ))}
              </div>
              <div className="event-list">
                <DiagnosticEventList events={snapshot?.diagnostics ?? []} />
              </div>
            </section>
          )}
        </>
      ) : (
        <section className="changes-panel" aria-label="BasiliskOS updates and changes">
          <div className="changes-head">
            <div><span className="zone-label">UPDATES</span><h2>{availableUpdate ? `${availableUpdate.name} is available` : "BasiliskOS is up to date"}</h2><p>Current version {APP_VERSION}</p></div>
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
                {release === availableUpdate && <button className="download-inline" onClick={() => void downloadUpdate(release)} disabled={busy !== null}><Download size={14} /> Install {release.tagName}</button>}
              </article>
            ))}
          </div>
        </section>
      )}

      {accountSwitchConfirm.open && (
        <div className="modal-backdrop" role="presentation" onClick={cancelAccountSwitch}>
          <div className="modal" role="alertdialog" aria-modal="true" aria-labelledby="account-switch-title" onClick={(event) => event.stopPropagation()}>
            <h3 id="account-switch-title">Switch account?</h3>
            <p>{accountSwitchConfirm.surface === "codex"
              ? "This will close and reopen the BasiliskOS Codex window. Any in-progress request in that window will be interrupted."
              : "This will close and reopen the BasiliskOS Claude window. Any in-progress request in that window will be interrupted."}</p>
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
            <h3 id="pending-confirm-title">BasiliskOS</h3>
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
            <p>{preparedUpdate.installerName} was downloaded and its SHA-256 checksum matched the published release manifest. BasiliskOS will close, then Windows will ask for administrator approval and open the installer. BasiliskOS itself does not stay elevated.</p>
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
            <p>Hide models you don't want cluttering the list. Once BasiliskOS has checked the backend for this provider, models it doesn't report as available are flagged automatically.</p>
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

      <footer>
        <p className={isError ? "error-message" : ""} aria-live="polite" aria-atomic="true">
          {message} {view === "console" && (
            <button className="activity-link" onClick={() => setShowDiagnostics((current) => !current)}>
              Activity {showDiagnostics ? "▾" : "▸"}
            </button>
          )}
        </p>
        <span>BasiliskOS {APP_VERSION} · CLIProxyAPI {snapshot?.version ?? "…"}</span>
      </footer>
    </main>
  );
}
