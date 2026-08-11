// Shared pure helpers used by both the main console (App.tsx) and the tray
// dashboard (TrayDashboard.tsx). Keep this module free of React and Tauri
// imports so either entry point can use it without pulling in the other's
// dependencies. Behavior here is unit-tested via App.test.tsx imports.

type ComponentStatus = { state: string; detail?: string };

export function messageFrom(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function thinkingLabel(value: string): string {
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

export function contextWindowLabel(tokens?: number): string | null {
  if (!tokens) return null;
  return `${Math.round(tokens / 1000)}K context`;
}

export function statusTone(
  status?: ComponentStatus,
): "healthy" | "pending" | "degraded" | "offline" {
  if (!status) return "offline";
  if (["running", "healthy", "selected", "ready", "completed"].includes(status.state)) {
    return "healthy";
  }
  if (["starting", "waiting"].includes(status.state)) return "pending";
  if (["degraded", "failed"].includes(status.state)) return "degraded";
  return "offline";
}
