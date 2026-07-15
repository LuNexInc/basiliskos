import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { DiagnosticEventList, StatusBadge } from "./App";

describe("truthful Basiliskos status components", () => {
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
});
