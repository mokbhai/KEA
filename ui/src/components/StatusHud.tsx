import { useEffect, useState } from "react";
import {
  onDictationError,
  onMeetingError,
  onMeetingVoiceSeparationUnavailable,
  onRewriteError,
  onRewriteProgress,
  onTtsError,
  onTtsState,
  setStatusOverlayVisible,
} from "../api";
import Spinner from "./Spinner";

type StatusState = {
  message: string;
  variant: "progress" | "error";
};

/**
 * Floating status indicator rendered in the transparent overlay window.
 *
 * Shows progress, completion ("Done"), and error feedback on top of all
 * windows and spaces (including full-screen apps) when global hotkeys or
 * system actions are triggered outside KEA.
 */
export default function StatusHud() {
  const [status, setStatus] = useState<StatusState | null>(null);

  useEffect(() => {
    let clearTimer: ReturnType<typeof setTimeout> | undefined;

    const showStatus = (message: string, variant: "progress" | "error") => {
      setStatus({ message, variant });
      void setStatusOverlayVisible(true);
      clearTimeout(clearTimer);

      const lower = message.toLowerCase();
      if (variant === "progress" && (lower === "done" || lower === "put your text back")) {
        clearTimer = setTimeout(() => {
          setStatus(null);
          void setStatusOverlayVisible(false);
        }, 3000);
      } else if (variant === "error") {
        clearTimer = setTimeout(() => {
          setStatus(null);
          void setStatusOverlayVisible(false);
        }, 4000);
      } else {
        // Safety timeout so in-flight status does not get stuck if an operation terminates unexpectedly
        clearTimer = setTimeout(() => {
          setStatus(null);
          void setStatusOverlayVisible(false);
        }, 15000);
      }
    };

    const unsubs = Promise.all([
      onRewriteProgress((message) => showStatus(message, "progress")),
      onRewriteError((message) => showStatus(message, "error")),
      onDictationError((message) => showStatus(message, "error")),
      onTtsState((state) => {
        if (state === "reading") {
          showStatus("Reading selection aloud…", "progress");
        } else if (state === "idle") {
          showStatus("Done", "progress");
        }
      }),
      onTtsError((message) => showStatus(message, "error")),
      onMeetingError((message) => showStatus(message, "error")),
      onMeetingVoiceSeparationUnavailable((reason) =>
        showStatus(`Voice separation is off for this meeting: ${reason}`, "progress"),
      ),
    ]);

    return () => {
      clearTimeout(clearTimer);
      void unsubs.then((fns) => fns.forEach((fn) => fn()));
    };
  }, []);

  if (!status) return null;

  const isDone =
    status.message.toLowerCase() === "done" ||
    status.message.toLowerCase() === "put your text back";

  return (
    <div
      role="status"
      className={`kea-hud kea-hud--status kea-status-pill kea-status-pill--${status.variant}`}
    >
      {status.variant === "error" && (
        <span className="kea-dot kea-dot--warn" aria-hidden="true" />
      )}
      {status.variant === "progress" && !isDone && (
        <Spinner size={12} />
      )}
      {status.variant === "progress" && isDone && (
        <span className="kea-dot kea-dot--ok" aria-hidden="true" />
      )}
      <span>{status.message}</span>
    </div>
  );
}
