import React from "react";
import { createRoot } from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import OverlayApp from "./OverlayApp";
import "./index.css";

const OVERLAY_LABEL = "overlay";

/**
 * Both windows load the same bundle; the label is what tells them apart. It
 * comes from an injected global, so a browser-only dev server (no Tauri) falls
 * back to the main window rather than throwing before anything renders.
 */
function currentWindowLabel(): string {
  try {
    return getCurrentWindow().label;
  } catch {
    return "main";
  }
}

const isOverlay = currentWindowLabel() === OVERLAY_LABEL;

// The overlay window is transparent; the stylesheet's opaque body would fill
// it back in. Set before the first paint so there is no flash of a grey box.
if (isOverlay) document.documentElement.dataset.window = OVERLAY_LABEL;

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>{isOverlay ? <OverlayApp /> : <App />}</React.StrictMode>,
);
