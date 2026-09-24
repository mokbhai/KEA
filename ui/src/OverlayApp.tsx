import AssistantHud from "./components/AssistantHud";
import DictationHud from "./components/DictationHud";
import { ThemeProvider } from "./theme";

/**
 * Root of the floating overlay window. The window itself is transparent and
 * borderless, so this renders only the HUD — no shell, no navigation.
 */
export default function OverlayApp() {
  return (
    <ThemeProvider>
      <div className="kea-overlay-root">
        <DictationHud />
        <AssistantHud />
      </div>
    </ThemeProvider>
  );
}
