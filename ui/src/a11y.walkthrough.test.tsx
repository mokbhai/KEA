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

const ACTION = {
  id: 7,
  feature_id: "rewrite",
  command: "rewrite_selection",
  engine_id: "openai",
  status: "ok",
};

const CONVERSATION = {
  id: 3,
  action_id: 7,
  feature_id: "rewrite",
  engine_id: "openai",
  model: "gpt-4o-mini",
  provider_ref: "openai",
  created_at: "2026-07-17T10:00:00Z",
};

const MEETING = {
  id: "meeting-1",
  title: "Test capture",
  started_at: "2026-07-17T10:00:00Z",
  ended_at: "2026-07-17T10:00:10Z",
  status: "done",
  capture_mode: "mic_only",
  stt_engine_id: "whisper",
  llm_engine_id: "openai",
  error: null,
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
        // Seeded, not empty: an empty table sweeps no rows, and the row
        // controls are exactly what needs covering.
        list_meetings: () => [MEETING],
        get_meeting: () => ({ meeting: MEETING, segments: [], notes: null }),
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
        list_actions: () => [ACTION],
        list_conversations: () => [CONVERSATION],
        list_messages: () => [],
        get_action: () => ({
          ...ACTION,
          model: "gpt-4o-mini",
          provider_ref: "openai",
          error: null,
          started_at: "2026-07-17T10:00:00Z",
          finished_at: "2026-07-17T10:00:01Z",
        }),
        tail_logs: () => "INFO ready",
        open_log_folder: () => undefined,
      },
    }),
  );
}

// <summary> is deliberately absent: browsers focus it, but user-event's tab()
// does not include it in its focusable set, so it would report false failures.
// The Advanced disclosures were checked by hand instead.
const CONTROL_SELECTOR = "button, a[href], input, select, textarea";

/**
 * Everything a sighted mouse user can operate on the page right now.
 *
 * Deliberately NOT filtered by tabindex/aria-hidden/inert: those are exactly
 * the ways a control gets hidden from the keyboard, so excluding them here
 * would let a regression delete itself from the expectation instead of failing
 * the test. They are asserted against separately below.
 */
function operableControls(): HTMLElement[] {
  return Array.from(document.body.querySelectorAll<HTMLElement>(CONTROL_SELECTOR)).filter(
    (el) => !el.hasAttribute("disabled"),
  );
}

/** Controls a mouse can reach but a keyboard cannot. */
function hiddenFromKeyboard(): HTMLElement[] {
  return operableControls().filter(
    (el) =>
      el.getAttribute("tabindex") === "-1" ||
      el.closest("[aria-hidden='true']") !== null ||
      el.closest("[inert]") !== null,
  );
}

const describeControl = (el: HTMLElement) =>
  `${el.tagName.toLowerCase()}: ${el.getAttribute("aria-label") ?? el.textContent?.trim()}`;

/**
 * Tabs from the top of the document and reports which controls focus never
 * lands on — the automated half of the keyboard-only walkthrough. A control
 * that never receives focus is either unreachable or sitting behind a trap.
 */
async function tabSweep(): Promise<HTMLElement[]> {
  const controls = operableControls();
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

/** Page title, how to render it, and row controls that must be in the sweep. */
const PAGES: [string, () => JSX.Element, RegExp[]][] = [
  ["Rewrite", () => <RewritePage />, []],
  ["Dictation", () => <DictationPage />, []],
  ["Meetings", () => <MeetingsPage />, [/Test capture/]],
  ["Read aloud", () => <ReadAloudPage />, []],
  ["AI Providers", () => <AiProvidersPage />, []],
  ["Models", () => <ModelsPage />, []],
  ["General", () => <GeneralPage onRunSetup={() => {}} />, []],
  ["History", () => <HistoryPage />, [/^Show action 7$/]],
  ["Logs", () => <LogsPage />, []],
];

describe("keyboard-only walkthrough", () => {
  beforeEach(() => {
    resetTauriMocks();
    mockWholeApp();
  });

  it.each(PAGES)(
    "reaches every control on %s with Tab alone",
    async (title, page, required) => {
      render(<ThemeProvider>{page()}</ThemeProvider>);
      await screen.findByRole("heading", { level: 1, name: title });
      // Let the page settle so late-arriving controls are part of the sweep.
      await waitFor(() => expect(invokeMock).toHaveBeenCalled());
      // Prove the seeded rows actually rendered, so the sweep is covering
      // their controls rather than an empty table.
      for (const name of required) {
        expect(await screen.findByRole("button", { name })).toBeTruthy();
      }

      expect(hiddenFromKeyboard().map(describeControl)).toEqual([]);
      expect((await tabSweep()).map(describeControl)).toEqual([]);
    },
  );

  it("reaches every control on History's conversations tab", async () => {
    render(<HistoryPage />);
    await userEvent.click(await screen.findByRole("button", { name: "Conversations" }));
    expect(
      await screen.findByRole("button", { name: "Show conversation 3" }),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "Delete conversation 3" })).toBeTruthy();

    expect(hiddenFromKeyboard().map(describeControl)).toEqual([]);
    expect((await tabSweep()).map(describeControl)).toEqual([]);
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
      expect(hiddenFromKeyboard().map(describeControl)).toEqual([]);
      expect((await tabSweep()).map(describeControl)).toEqual([]);

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
