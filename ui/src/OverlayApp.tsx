import { useEffect, useState } from "react";
import {
  onOverlayOwner,
  overlayReady,
  type OverlayOwnership,
} from "./api";
import AssistantHud from "./components/AssistantHud";
import DictationHud from "./components/DictationHud";
import StatusHud from "./components/StatusHud";
import { ThemeProvider } from "./theme";

const NOBODY: OverlayOwnership = {
  owner: null,
  dictation: false,
  assistant: false,
  status: false,
};

/**
 * Root of the floating overlay window. The window itself is transparent and
 * borderless, so this renders only the HUD — no shell, no navigation.
 *
 * **One window, three features, one visible at a time.** Dictation, the voice
 * assistant and status notices all draw into this webview, and the Rust side
 * (`overlay::Ownership`) decides which of them owns it: the one with the most
 * recent activity. It broadcasts that decision as `overlay:owner`, and this
 * component only obeys it. Before, all three rendered whatever they last heard,
 * side by side, so an assistant session that ended without a closing event left
 * its "Listening…" pill on screen beside every later dictation run.
 *
 * Every HUD stays mounted even while it is not the owner. They are what
 * subscribe to their own feature's events, and unmounting one would drop the
 * events of a session still in progress underneath a newer owner.
 *
 * What `assistant` (the claim flag, not the owner) controls is the assistant's
 * *memory*: it is wiped when the session lets go of the overlay, not when
 * something newer merely covers it.
 */
export default function OverlayApp() {
  const [ownership, setOwnership] = useState<OverlayOwnership>(NOBODY);

  useEffect(() => {
    let live = true;
    const sub = onOverlayOwner((next) => {
      if (live) setOwnership(next);
    });
    // The snapshot covers an event that fired before this listener existed —
    // including after the webview reloaded, which is when the Rust side's
    // memory of what to draw is all there is.
    overlayReady()
      .then((snapshot) => {
        if (live && snapshot) setOwnership(snapshot);
      })
      .catch(() => {});
    return () => {
      live = false;
      void sub.then((un) => un());
    };
  }, []);

  return (
    <ThemeProvider>
      <div className="kea-overlay-root">
        <DictationHud visible={ownership.owner === "dictation"} />
        <AssistantHud
          visible={ownership.owner === "assistant"}
          sessionOpen={ownership.assistant}
        />
        <StatusHud visible={ownership.owner === "status"} />
      </div>
    </ThemeProvider>
  );
}
