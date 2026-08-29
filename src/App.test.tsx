import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import {
  accountNeedsRelogin,
  credentialAlert,
  DiagnosticEventList,
  isNewerVersion,
  prefersManualAuthBrowser,
  StatusBadge,
  usageAccountFiles,
  usageResetLabel,
} from "./App";

describe("truthful BasiliskOS status components", () => {
  it("only advertises a numerically newer published version", () => {
    expect(isNewerVersion("v1.1.16", "1.1.15")).toBe(true);
    expect(isNewerVersion("v1.1.15", "1.1.15")).toBe(false);
    expect(isNewerVersion("v1.1.9", "1.1.15")).toBe(false);
  });
  it("renders a verified healthy backend as healthy", () => {
    render(<StatusBadge label="Engine" status={{ state: "healthy", detail: "Authenticated health check passed" }} />);
    expect(screen.getByText("Engine · healthy")).toHaveClass("healthy");
  });

  it("renders degraded and offline services without claiming they are running", () => {
    const { rerender } = render(<StatusBadge label="Engine" status={{ state: "degraded", detail: "Backend exited" }} />);
    expect(screen.getByText("Engine · degraded")).toHaveClass("degraded");
    rerender(<StatusBadge label="Engine" status={{ state: "stopped", detail: "Relay stopped" }} />);
    expect(screen.getByText("Engine · stopped")).toHaveClass("offline");
  });

  it("shows a cancellable OAuth wait state as pending", () => {
    render(<StatusBadge label="OAuth" status={{ state: "waiting", detail: "Waiting for provider" }} />);
    expect(screen.getByText("OAuth · waiting")).toHaveClass("pending");
  });

  it("shows missing credentials after account removal", () => {
    render(<StatusBadge label="Credential" status={{ state: "missing", detail: "No active credential" }} />);
    expect(screen.getByText("Credential · missing")).toHaveClass("offline");
  });

  it("shows backend crash followed by recovery", () => {
    const { rerender } = render(<StatusBadge label="Backend" status={{ state: "degraded", detail: "Restart scheduled" }} />);
    expect(screen.getByText("Backend · degraded")).toBeInTheDocument();
    rerender(<StatusBadge label="Backend" status={{ state: "healthy", detail: "Restart completed" }} />);
    expect(screen.getByText("Backend · healthy")).toHaveClass("healthy");
  });

  it("renders a stable stale-auth code without secrets or prompt content", () => {
    render(<DiagnosticEventList events={[{
      timestamp: "2026-07-15T08:00:00Z",
      code: "BAS-UPSTREAM-001",
      severity: "warning",
      message: "The provider rejected the selected credential.",
      httpStatus: 401,
      provider: "codex",
    }]} />);
    expect(screen.getByText("BAS-UPSTREAM-001")).toBeInTheDocument();
    expect(screen.getByText("The provider rejected the selected credential.")).toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/token|prompt|bearer/i);
  });

  it("shows actionable login warnings without displaying token-expiry timestamps", () => {
    const base = {
      fileName: "kimi-account.json",
      provider: "kimi" as const,
      label: "Kimi account",
      disabled: false,
      active: false,
      activeForCodex: false,
      credentialStatus: "relogin_required" as const,
    };
    expect(credentialAlert(base, Date.now())).toEqual({ label: "Sign in again", tone: "relogin" });
    expect(credentialAlert({ ...base, credentialStatus: "unknown" }, Date.now())).toBeUndefined();
    expect(credentialAlert({
      ...base,
      credentialStatus: "renewal_due",
      expiresAtMs: Date.now() + 60_000,
    }, Date.now())).toEqual({ tone: "renewal", label: "Login refresh needed" });
    expect(credentialAlert({
      ...base,
      credentialStatus: "active",
      expiresAtMs: Date.now() + 60_000,
    }, Date.now())).toBeUndefined();
    expect(accountNeedsRelogin(base)).toBe(true);
    expect(accountNeedsRelogin({ credentialStatus: "active" })).toBe(false);
    expect(accountNeedsRelogin(
      { credentialStatus: "active" },
      "Usage check unavailable — saved login is active. Auto-retry in 5 min or use Refresh usage.",
    )).toBe(false);
    expect(accountNeedsRelogin(
      { credentialStatus: "active" },
      "Codex refresh grant was revoked. Re-login once to restore automatic refresh.",
    )).toBe(true);
  });

  it("keeps provider renewal time separate from login-token expiry", () => {
    const renewal = new Date(2026, 7, 7, 23, 36, 28).getTime();
    expect(usageResetLabel(renewal)).toMatch(/^Renews .*Aug.*7.*11:36.*PM$/i);
    expect(usageResetLabel(undefined)).toBeUndefined();
  });

  it("prefers manual browser open for multi-account cookie-prone providers only", () => {
    expect(prefersManualAuthBrowser("xai")).toBe(true);
    expect(prefersManualAuthBrowser("kimi")).toBe(true);
    expect(prefersManualAuthBrowser("codex")).toBe(false);
    expect(prefersManualAuthBrowser("claude")).toBe(false);
    expect(prefersManualAuthBrowser("zai")).toBe(true);
    expect(prefersManualAuthBrowser("antigravity")).toBe(false);
  });

  it("refreshes usage for every OAuth account through one global action", () => {
    expect(usageAccountFiles([
      { fileName: "codex-a.json", provider: "codex" },
      { fileName: "xai-b.json", provider: "xai" },
      { fileName: "kimi-d.json", provider: "kimi" },
      { fileName: "antigravity-e.json", provider: "antigravity" },
      { fileName: "zai-f.json", provider: "zai" },
    ])).toEqual(["codex-a.json", "xai-b.json", "kimi-d.json"]);
  });
});
