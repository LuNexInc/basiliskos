import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

// The tray dashboard renders a static preview snapshot when the URL carries
// `?tray`. We set that before the module is imported so PREVIEW_TRAY is true.
async function loadTrayWithTrayFlag() {
  window.history.replaceState({}, "", "?tray");
  const { default: TrayDashboard } = await import("./TrayDashboard");
  return TrayDashboard;
}

describe("tray dashboard preview render", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    window.history.replaceState({}, "", window.location.pathname);
  });

  it("renders the header and health statuses", async () => {
    const TrayDashboard = await loadTrayWithTrayFlag();
    render(<TrayDashboard />);
    await act(async () => {});
    expect(screen.getByText("BasiliskOS")).toBeInTheDocument();
    expect(screen.getByText("Relay")).toBeInTheDocument();
    expect(screen.getByText("Online")).toBeInTheDocument();
    expect(screen.getByText("Relay running")).toBeInTheDocument();
    expect(screen.getByText("Link healthy")).toBeInTheDocument();
    expect(screen.getAllByText("Claude window open").length).toBeGreaterThan(0);
  });

  it("shows the primary service route and the quick actions", async () => {
    const TrayDashboard = await loadTrayWithTrayFlag();
    render(<TrayDashboard />);
    await act(async () => {});
    expect(screen.getAllByText("Claude Code").length).toBeGreaterThan(0);
    expect(screen.getAllByText("GPT-5.4").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Codex worker").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Primary").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Stop relay").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Close Claude").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Open full window").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Quit BasiliskOS").length).toBeGreaterThan(0);
  });
});
