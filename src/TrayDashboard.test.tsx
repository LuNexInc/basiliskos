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

  it("renders the fuel-core header and health statuses", async () => {
    const TrayDashboard = await loadTrayWithTrayFlag();
    render(<TrayDashboard />);
    await act(async () => {});
    expect(screen.getByText("Basiliskos")).toBeInTheDocument();
    expect(screen.getByText("Fuel core")).toBeInTheDocument();
    expect(screen.getByText("CORE ONLINE")).toBeInTheDocument();
    expect(screen.getByText(/Relay · /)).toBeInTheDocument();
    expect(screen.getByText(/Link · /)).toBeInTheDocument();
    expect(screen.getByText(/Claude · /)).toBeInTheDocument();
  });

  it("shows the primary service route and the quick actions", async () => {
    const TrayDashboard = await loadTrayWithTrayFlag();
    render(<TrayDashboard />);
    await act(async () => {});
    expect(screen.getAllByText("Claude Code").length).toBeGreaterThan(0);
    expect(screen.getAllByText("PRIMARY").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Stop relay").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Close Claude").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Open full window").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Quit Basiliskos").length).toBeGreaterThan(0);
  });
});
