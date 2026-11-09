import { act, render } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { emitTauriEvent, onInvoke, resetTauriMocks } from "./test-utils/tauri";
import OverlayApp from "./OverlayApp";

vi.mock("@tauri-apps/api/core", async () => (await import("./test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("./test-utils/tauri")).eventModule);

type Owner = "dictation" | "assistant" | "status" | null;

/** What the Rust side broadcasts whenever a feature claims or releases the overlay. */
function ownership(owner: Owner, active: Partial<Record<"dictation" | "assistant" | "status", boolean>> = {}) {
  return {
    owner,
    dictation: owner === "dictation",
    assistant: owner === "assistant",
    status: owner === "status",
    ...active,
  };
}

const assistantHud = () => document.querySelector(".kea-hud--assistant");
const dictationHud = () => document.querySelector(".kea-hud--listening, .kea-hud--processing, .kea-hud--locked");
const statusHud = () => document.querySelector(".kea-hud--status");

async function renderOverlay() {
  onInvoke({
    overlay_ready: () => ownership(null),
    get_assistant_settings: () => ({ speak_answers: true, show_answers: true }),
    set_status_overlay_visible: () => null,
  });
  const view = render(<OverlayApp />);
  await act(async () => {});
  return view;
}

const emit = (event: string, payload: unknown) =>
  act(async () => {
    emitTauriEvent(event, payload);
  });

describe("OverlayApp ownership", () => {
  beforeEach(() => {
    resetTauriMocks();
  });

  it("renders nothing until some feature owns the overlay", async () => {
    await renderOverlay();
    expect(assistantHud()).toBeNull();
    expect(dictationHud()).toBeNull();
    expect(statusHud()).toBeNull();
  });

  it("shows the assistant while it owns the overlay", async () => {
    await renderOverlay();
    await emit("overlay:owner", ownership("assistant"));
    await emit("assistant:state", { state: "listening" });
    expect(assistantHud()).not.toBeNull();
  });

  it("does not draw a cancelled assistant's 'Listening…' beside a dictation run", async () => {
    // The reported screenshot: an assistant session cancelled with Escape
    // while listening emits no further state, so its last pill stayed mounted.
    await renderOverlay();
    await emit("overlay:owner", ownership("assistant"));
    await emit("assistant:state", { state: "listening" });
    await emit("overlay:owner", ownership(null)); // session ended, released

    await emit("overlay:owner", ownership("dictation"));
    await emit("dictation:state", { state: "processing" });

    expect(dictationHud()).not.toBeNull();
    expect(assistantHud()).toBeNull();
  });

  it("shows only the most recent owner when two are active", async () => {
    await renderOverlay();
    await emit("overlay:owner", ownership("assistant"));
    await emit("assistant:state", { state: "presenting" });
    await emit("overlay:owner", ownership("dictation", { assistant: true }));
    await emit("dictation:state", { state: "listening" });

    expect(dictationHud()).not.toBeNull();
    expect(assistantHud()).toBeNull();
  });

  it("brings the assistant back, with its answer, when the newer owner lets go", async () => {
    await renderOverlay();
    await emit("overlay:owner", ownership("assistant"));
    await emit("assistant:state", { state: "presenting" });
    await emit("assistant:answer", { request: "capital of France", text: "Paris." });
    await emit("overlay:owner", ownership("dictation", { assistant: true }));
    await emit("overlay:owner", ownership("assistant"));

    expect(assistantHud()?.textContent).toContain("Paris.");
  });

  it("drops the assistant's answer once the session releases, so the next session starts clean", async () => {
    await renderOverlay();
    await emit("overlay:owner", ownership("assistant"));
    await emit("assistant:state", { state: "presenting" });
    await emit("assistant:answer", { request: "capital of France", text: "Paris." });
    await emit("overlay:owner", ownership(null));

    // A new session claims the overlay before its first state arrives.
    await emit("overlay:owner", ownership("assistant"));
    expect(assistantHud()).toBeNull();
    expect(document.body.textContent).not.toContain("Paris.");
  });

  it("shows a status notice while it is the newest owner", async () => {
    await renderOverlay();
    await emit("overlay:owner", ownership("status"));
    await emit("rewrite:error", { message: "Boom" });
    expect(statusHud()).not.toBeNull();
  });

  it("shows the 'AI cleanup was skipped' notice a dictation run raises as it finishes", async () => {
    await renderOverlay();
    await emit("overlay:owner", ownership("dictation"));
    await emit("dictation:state", { state: "processing" });
    // The backend claims the overlay for the notice, then the run goes idle and
    // releases only dictation's claim.
    await emit("overlay:owner", ownership("status", { dictation: true }));
    await emit("dictation:error", { message: "AI cleanup was skipped — typed your raw transcript." });
    await emit("dictation:state", { state: "idle" });
    await emit("overlay:owner", ownership("status"));

    expect(statusHud()?.textContent).toContain("AI cleanup was skipped");
    expect(dictationHud()).toBeNull();
  });

  it("keeps showing the second recording while the first run finishes underneath it", async () => {
    await renderOverlay();
    await emit("overlay:owner", ownership("dictation"));
    await emit("dictation:state", { state: "processing" }); // run A
    await emit("dictation:state", { state: "listening" }); // run B starts
    // Run A finishes while B records: the backend publishes nothing.
    expect(dictationHud()?.className).toContain("kea-hud--listening");
    await emit("dictation:state", { state: "processing" }); // run B stops
    expect(dictationHud()?.className).toContain("kea-hud--processing");
  });

  it("returns to the recording once a notice raised during it has had its dwell", async () => {
    await renderOverlay();
    await emit("overlay:owner", ownership("dictation"));
    await emit("dictation:state", { state: "listening" });
    await emit("overlay:owner", ownership("status", { dictation: true }));
    await emit("dictation:error", { message: "AI cleanup was skipped" });
    expect(dictationHud()).toBeNull();
    await emit("overlay:owner", ownership("dictation"));

    expect(dictationHud()?.className).toContain("kea-hud--listening");
  });

  it("applies the snapshot the backend returns on load", async () => {
    onInvoke({
      overlay_ready: () => ownership("dictation"),
      get_assistant_settings: () => ({ speak_answers: true, show_answers: true }),
      set_status_overlay_visible: () => null,
    });
    render(<OverlayApp />);
    await act(async () => {});
    await emit("dictation:state", { state: "listening" });
    expect(dictationHud()).not.toBeNull();
  });
});
