import React from "react";
import { createRoot } from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import OverlayApp from "./OverlayApp";
import PaletteApp from "./components/PaletteApp";
import "./index.css";

const OVERLAY_LABEL = "overlay";
const PALETTE_LABEL = "palette";

/**
 * All three windows load the same bundle; the label is what tells them apart.
 * It comes from an injected global, so a browser-only dev server (no Tauri)
 * falls back to the main window rather than throwing before anything renders.
 */
function currentWindowLabel(): string {
  try {
    return getCurrentWindow().label;
  } catch {
    return "main";
  }
}

const label = currentWindowLabel();
const isOverlay = label === OVERLAY_LABEL;
const isPalette = label === PALETTE_LABEL;

// Both secondary windows are transparent; the stylesheet's opaque body would
// fill them back in. Set before the first paint so there is no flash of a grey
// box. The palette sets the background itself rather than relying on a
// stylesheet rule, because it is not the overlay and must not pretend to be:
// `:root[data-window="overlay"]` also drives the HUD's own layout rules.
if (isOverlay || isPalette) {
  document.documentElement.dataset.window = label;
}
if (isPalette) {
  document.documentElement.style.background = "transparent";
  document.body.style.background = "transparent";
}

function Root() {
  if (isOverlay) return <OverlayApp />;
  if (isPalette) return <PaletteApp />;
  return <App />;
}

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>,
);
