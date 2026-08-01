import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeMock, onInvoke, resetTauriMocks } from "./test-utils/tauri";
import { featureHandlers, openAiBinding } from "./test-utils/featureWorld";
import Onboarding from "./components/Onboarding";
import AiProvidersPage from "./pages/AiProvidersPage";
import DictationPage from "./pages/DictationPage";
import GeneralPage from "./pages/GeneralPage";
import HistoryPage from "./pages/HistoryPage";
import LogsPage from "./pages/LogsPage";
import MeetingsPage from "./pages/MeetingsPage";
import ModelsPage from "./pages/ModelsPage";
import ReadAloudPage from "./pages/ReadAloudPage";
import RewritePage from "./pages/RewritePage";
import { ThemeProvider } from "./theme";

vi.mock("@tauri-apps/api/core", async () => (await import("./test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("./test-utils/tauri")).eventModule);

const whisperBinding = {
  engine_id: "whisper",
  model: "whisper-base",
  provider_ref: null,
};

/** Every page's backend, resolved and healthy, so nothing is hidden behind an error. */
function mockWholeApp() {
  onInvoke(
    featureHandlers({
      engines: { llm: ["openai"], stt: ["whisper"], tts: ["openai-tts"] },
      bindings: {
        "default/llm": openAiBinding("openai", "gpt-4o-mini"),
        "default/stt": whisperBinding,
        "default/tts": openAiBinding("openai-tts"),
      },
      extra: {
        get_setting: () => null,
        set_setting: () => undefined,
        list_presets: () => [],
        get_prompt_override: () => null,
        get_dictation_state: () => "idle",
        get_autostart: () => false,
        set_autostart: () => undefined,
        get_all_permission_statuses: () => [
          { kind: "microphone", status: "Granted" },
          { kind: "screen_recording", status: "Granted" },
          { kind: "accessibility", status: "Granted" },
        ],
        get_permission_status: () => "Granted",
        request_permission: () => "Granted",
        list_meetings: () => [],
        get_meeting_state: () => ({ state: "idle", active_meeting_id: null }),
        get_meeting_settings: () => ({
          segment_duration_secs: 30,
          prefer_system_audio: false,
        }),
        set_meeting_settings: () => undefined,
        get_system_audio_capability: () => "mic_only",
        preview_voice: () => undefined,
        test_provider: () => "ok",
        delete_model: () => undefined,
        list_actions: () => [],
        list_conversations: () => [],
        list_messages: () => [],
        get_action: () => null,
        tail_logs: () => "INFO ready",
        open_log_folder: () => undefined,
      },
    }),
  );
}

// <summary> is deliberately absent: browsers focus it, but user-event's tab()
// does not include it in its focusable set, so it would report false failures.
// The Advanced disclosures were checked by hand instead.
const CONTROL_SELECTOR = "button, a[href], input, select, textarea, [tabindex]";

/** Anything a sighted mouse user can operate on the page right now. */
function enabledControls(): HTMLElement[] {
  return Array.from(document.body.querySelectorAll<HTMLElement>(CONTROL_SELECTOR)).filter(
    (el) =>
      !el.hasAttribute("disabled") &&
      el.getAttribute("aria-hidden") !== "true" &&
      el.getAttribute("tabindex") !== "-1" &&
      !el.closest("[aria-hidden='true']") &&
      !el.closest("[inert]"),
  );
}

/**
 * Tabs from the top of the document and reports which controls focus never
 * lands on — the automated half of the keyboard-only walkthrough. A control
 * that never receives focus is either unreachable or sitting behind a trap.
 */
async function tabSweep(): Promise<HTMLElement[]> {
  const controls = enabledControls();
  const seen = new Set<Element>();
  const seenRadioGroups = new Set<string>();
  document.body.focus();
  // One extra pass so a wrap-around still visits everything.
  for (let i = 0; i < controls.length + 2; i += 1) {
    await userEvent.tab();
    const active = document.activeElement as HTMLInputElement | null;
    if (!active || active === document.body) continue;
    seen.add(active);
    if (active.type === "radio" && active.name) seenRadioGroups.add(active.name);
  }
  return controls.filter((el) => {
    if (seen.has(el)) return false;
    // A radio group is a single tab stop by design; its other members are
    // reached with the arrow keys, so the group counts as covered.
    const input = el as HTMLInputElement;
    if (input.type === "radio" && input.name) return !seenRadioGroups.has(input.name);
    return true;
  });
}

const PAGES: [string, () => JSX.Element][] = [
  ["Rewrite", () => <RewritePage />],
  ["Dictation", () => <DictationPage />],
  ["Meetings", () => <MeetingsPage />],
  ["Read aloud", () => <ReadAloudPage />],
  ["AI Providers", () => <AiProvidersPage />],
  ["Models", () => <ModelsPage />],
  ["General", () => <GeneralPage onRunSetup={() => {}} />],
  ["History", () => <HistoryPage />],
  ["Logs", () => <LogsPage />],
];

describe("keyboard-only walkthrough", () => {
  beforeEach(() => {
    resetTauriMocks();
    mockWholeApp();
  });

  it.each(PAGES)("reaches every control on %s with Tab alone", async (title, page) => {
    render(<ThemeProvider>{page()}</ThemeProvider>);
    await screen.findByRole("heading", { level: 1, name: title });
    // Let the page settle so late-arriving controls are part of the sweep.
    await waitFor(() => expect(invokeMock).toHaveBeenCalled());

    const unreachable = await tabSweep();

    expect(
      unreachable.map((el) => `${el.tagName.toLowerCase()}: ${el.textContent?.trim()}`),
    ).toEqual([]);
  });

  it("runs the wizard end to end from the keyboard", async () => {
    const onFinish = vi.fn();
    render(
      <ThemeProvider>
        <Onboarding onFinish={onFinish} />
      </ThemeProvider>,
    );

    await screen.findByRole("heading", { level: 1, name: "Welcome to KEA" });

    // Four steps, each advanced by focusing its primary action and pressing
    // Enter — no pointer events anywhere in this test.
    for (let step = 0; step < 4; step += 1) {
      const unreachable = await tabSweep();
      expect(
        unreachable.map(
          (el) =>
            `${el.tagName.toLowerCase()}[${el.getAttribute("type") ?? ""}] ${el.getAttribute("aria-label") ?? el.textContent?.trim()}`,
        ),
      ).toEqual([]);

      const next = screen.getByRole("button", {
        name: step === 3 ? "Finish" : "Continue",
      });
      next.focus();
      expect(document.activeElement).toBe(next);
      await userEvent.keyboard("{Enter}");
    }

    await waitFor(() => expect(onFinish).toHaveBeenCalled());
  });
});
