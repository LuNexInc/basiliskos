import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import TrayDashboard from "./TrayDashboard";

function windowLabel() {
  try {
    return getCurrentWindow().label;
  } catch {
    return "main";
  }
}

const root = document.getElementById("root") as HTMLElement;
const label = windowLabel();
const forceTray =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).has("tray");

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    {label === "tray-dashboard" || forceTray ? <TrayDashboard /> : <App />}
  </React.StrictMode>,
);
