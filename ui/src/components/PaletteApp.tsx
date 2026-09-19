import { ThemeProvider } from "../theme";
import PromptPalette from "./PromptPalette";

/**
 * Root of the prompt palette window — the third branch of the one bundle,
 * beside `App` and `OverlayApp`.
 *
 * A second Vite entry was the alternative and it costs more than it saves: a
 * duplicated theme-bootstrap script in a second HTML file and a second CSS
 * graph, to avoid loading a bundle that is already in memory. The palette
 * window is built hidden at startup, so what it carries is paid once at launch
 * and never on the open path. If it ever shows up as memory pressure the
 * answer is `React.lazy` per branch, not a second document.
 *
 * The centring lives here rather than in the palette itself so the card stays
 * a plain block that a test can render on its own.
 */
export default function PaletteApp() {
  return (
    <ThemeProvider>
      <div
        style={{
          display: "flex",
          alignItems: "flex-start",
          justifyContent: "center",
          width: "100vw",
          padding: 12,
          boxSizing: "border-box",
        }}
      >
        <PromptPalette />
      </div>
    </ThemeProvider>
  );
}
