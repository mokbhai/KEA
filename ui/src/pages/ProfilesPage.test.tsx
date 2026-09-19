import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  invokeCalls,
  onInvoke,
  resetTauriMocks,
} from "../test-utils/tauri";
import type { AppProfile } from "../api";
import ProfilesPage from "./ProfilesPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

const profile = (over: Partial<AppProfile> = {}): AppProfile => ({
  id: "profile-slack",
  name: "Slack",
  enabled: true,
  priority: 0,
  match_bundle_id: "com.tinyspeck.slackmacgap",
  match_url_glob: null,
  rewrite_mode: "friendly",
  preset_id: null,
  llm_engine_id: null,
  llm_model: null,
  llm_provider_ref: null,
  post_process: null,
  insertion_mode: null,
  created_at: "2026-09-19T10:00:00Z",
  ...over,
});

type Handlers = Parameters<typeof onInvoke>[0];

/** The page's backend: profiles as given, everything else quiet and empty. */
function mockWorld({
  profiles = [] as AppProfile[],
  settings = {} as Record<string, string>,
  extra = {} as Handlers,
} = {}) {
  // The backend orders by priority, and the page re-sorts; returning them
  // already sorted would hide a page that forgot to.
  let stored = [...profiles];
  onInvoke({
    list_app_profiles: () => stored,
    upsert_app_profile: (args) => {
      const next = (args as { profile: AppProfile }).profile;
      stored = [...stored.filter((p) => p.id !== next.id), next];
    },
    delete_app_profile: (args) => {
      stored = stored.filter((p) => p.id !== (args as { id: string }).id);
    },
    capture_app_context: () => null,
    get_setting: (args) => settings[(args as { key: string }).key] ?? null,
    set_setting: () => undefined,
    list_presets: () => [{ id: "preset-1", name: "Standup", instruction: "be brief" }],
    list_providers: () => [{ provider_ref: "openai", name: "OpenAI", built_in: true }],
    ...extra,
  });
}

/** The Rule column of every listed row, top to bottom. */
const ruleNames = () =>
  screen
    .getAllByRole("row")
    .slice(1)
    .map((row) => (row as HTMLTableRowElement).cells[1].textContent);

const savedProfile = (index = 0) =>
  (invokeCalls("upsert_app_profile")[index] as { profile: AppProfile }).profile;

describe("ProfilesPage", () => {
  beforeEach(() => resetTauriMocks());

  it("titles the page with the only h1", async () => {
    mockWorld();
    render(<ProfilesPage />);

    expect(
      await screen.findByRole("heading", { level: 1, name: "App profiles" }),
    ).toBeTruthy();
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
  });

  it("says what profiles are for when there are none", async () => {
    mockWorld();
    render(<ProfilesPage />);

    expect(await screen.findByText(/No rules yet/)).toBeTruthy();
  });

  it("lists a rule with what it matches and what it changes", async () => {
    mockWorld({
      profiles: [
        profile({ post_process: false, insertion_mode: "paste" }),
      ],
    });
    render(<ProfilesPage />);

    const row = (await screen.findByRole("cell", { name: "Slack" })).closest("tr")!;
    expect(within(row).getByText("com.tinyspeck.slackmacgap")).toBeTruthy();
    expect(within(row).getByText(/Friendly · no AI clean-up · Paste/)).toBeTruthy();
  });

  it("writes a new rule with every override left inheriting", async () => {
    mockWorld();
    render(<ProfilesPage />);

    await userEvent.click(await screen.findByRole("button", { name: "New rule" }));
    await userEvent.type(screen.getByRole("textbox", { name: "Rule name" }), "Terminal");
    await userEvent.type(
      screen.getByRole("textbox", { name: "Bundle id" }),
      "com.apple.Terminal",
    );
    await userEvent.click(screen.getByRole("button", { name: "Save rule" }));

    await waitFor(() => expect(invokeCalls("upsert_app_profile")).toHaveLength(1));
    expect(savedProfile()).toMatchObject({
      name: "Terminal",
      enabled: true,
      match_bundle_id: "com.apple.Terminal",
      match_url_glob: null,
      rewrite_mode: null,
      preset_id: null,
      post_process: null,
      insertion_mode: null,
      llm_engine_id: null,
    });
    // The editor closes and the saved rule is listed.
    expect(await screen.findByRole("cell", { name: "Terminal" })).toBeTruthy();
  });

  describe("AI clean-up is a tri-state", () => {
    const openEditor = async () => {
      render(<ProfilesPage />);
      await userEvent.click(await screen.findByRole("button", { name: "Edit Slack" }));
      return screen.getByRole("combobox", { name: "AI clean-up" }) as HTMLSelectElement;
    };

    it("offers inherit, always and never as three separate choices", async () => {
      mockWorld({ profiles: [profile()] });
      const select = await openEditor();

      expect(select.value).toBe("inherit");
      expect(
        Array.from(select.options).map((o) => o.value),
      ).toEqual(["inherit", "on", "off"]);
    });

    it("stores false for never, not null", async () => {
      mockWorld({ profiles: [profile()] });
      const select = await openEditor();

      await userEvent.selectOptions(select, "off");
      await userEvent.click(screen.getByRole("button", { name: "Save rule" }));

      await waitFor(() => expect(invokeCalls("upsert_app_profile")).toHaveLength(1));
      // false is "never clean up", null is "inherit" — collapsing them would
      // turn clean-up off for every app that never opted in.
      expect(savedProfile().post_process).toBe(false);
    });

    it("stores null again when set back to inherit", async () => {
      mockWorld({ profiles: [profile({ post_process: true })] });
      const select = await openEditor();
      expect(select.value).toBe("on");

      await userEvent.selectOptions(select, "inherit");
      await userEvent.click(screen.getByRole("button", { name: "Save rule" }));

      await waitFor(() => expect(invokeCalls("upsert_app_profile")).toHaveLength(1));
      expect(savedProfile().post_process).toBeNull();
    });
  });

  it("fills in the bundle id the capture found and says what it got", async () => {
    mockWorld({
      profiles: [profile()],
      extra: {
        capture_app_context: () => ({
          bundle_id: "com.apple.Terminal",
          app_name: "Terminal",
          window_title: "zsh",
          url: null,
        }),
      },
    });
    render(<ProfilesPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Edit Slack" }));
    await userEvent.click(screen.getByRole("button", { name: "Use the app I switch to" }));

    await waitFor(() =>
      expect(
        (screen.getByRole("textbox", { name: "Bundle id" }) as HTMLInputElement).value,
      ).toBe("com.apple.Terminal"),
    );
    // Which app it read is shown, because the button cannot promise it read
    // the one the user meant.
    expect(screen.getByText(/Captured Terminal — com.apple.Terminal/)).toBeTruthy();
  });

  it("explains a capture that identified nothing instead of writing an empty rule", async () => {
    mockWorld({ profiles: [profile()] });
    render(<ProfilesPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Edit Slack" }));
    await userEvent.click(screen.getByRole("button", { name: "Use the app I switch to" }));

    expect(
      await screen.findByText(/KEA was still the app in front/),
    ).toBeTruthy();
    expect(
      (screen.getByRole("textbox", { name: "Bundle id" }) as HTMLInputElement).value,
    ).toBe("com.tinyspeck.slackmacgap");
  });

  it("reports a failed capture through the error line", async () => {
    mockWorld({
      profiles: [profile()],
      extra: {
        capture_app_context: () => {
          throw new Error("accessibility permission missing");
        },
      },
    });
    render(<ProfilesPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Edit Slack" }));
    await userEvent.click(screen.getByRole("button", { name: "Use the app I switch to" }));

    expect(await screen.findByText(/accessibility permission missing/)).toBeTruthy();
  });

  it("lists rules in the order the backend resolves them", async () => {
    mockWorld({
      profiles: [
        profile({ id: "p-any", name: "Everywhere", match_bundle_id: null, priority: 99 }),
        profile({
          id: "p-both",
          name: "Slack web",
          match_url_glob: "*.slack.com/*",
          priority: 0,
        }),
        profile({ id: "p-app", name: "Slack app", priority: 50 }),
      ],
    });
    render(<ProfilesPage />);

    await screen.findByRole("cell", { name: "Slack web" });
    // Specificity first, priority only within a band — so the catch-all sits
    // last however high its number.
    expect(
      ruleNames(),
    ).toEqual(["Slack web", "Slack app", "Everywhere"]);
  });

  it("reorders two equally specific rules and renumbers their priorities", async () => {
    mockWorld({
      profiles: [
        profile({ id: "p-a", name: "First", priority: 2 }),
        profile({ id: "p-b", name: "Second", priority: 1 }),
      ],
    });
    render(<ProfilesPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Move Second up" }));

    await waitFor(() => expect(invokeCalls("upsert_app_profile").length).toBeGreaterThan(0));
    const saved = invokeCalls("upsert_app_profile").map(
      (args) => (args as { profile: AppProfile }).profile,
    );
    const priorityOf = (id: string) => saved.find((p) => p.id === id)?.priority;
    expect(priorityOf("p-b")).toBeGreaterThan(priorityOf("p-a") ?? 0);
    await waitFor(() =>
      expect(
        ruleNames(),
      ).toEqual(["Second", "First"]),
    );
  });

  it("does not offer a move that the resolver would undo", async () => {
    mockWorld({
      profiles: [
        profile({ id: "p-app", name: "Slack app" }),
        profile({ id: "p-any", name: "Everywhere", match_bundle_id: null }),
      ],
    });
    render(<ProfilesPage />);

    // Priority never lifts a catch-all above a rule that names an app, so the
    // arrow that would claim otherwise is off.
    const up = await screen.findByRole("button", { name: "Move Everywhere up" });
    expect(up.hasAttribute("disabled")).toBe(true);
    expect(
      screen.getByRole("button", { name: "Move Slack app down" }).hasAttribute("disabled"),
    ).toBe(true);
  });

  it("turns a rule off without deleting it, and puts it back if the write fails", async () => {
    mockWorld({
      profiles: [profile()],
      extra: {
        upsert_app_profile: () => {
          throw new Error("database is locked");
        },
      },
    });
    render(<ProfilesPage />);

    const toggle = await screen.findByRole("switch", { name: "Use Slack" });
    await userEvent.click(toggle);

    expect(await screen.findByText(/database is locked/)).toBeTruthy();
    expect(toggle.getAttribute("aria-checked")).toBe("true");
  });

  it("deletes a rule", async () => {
    mockWorld({ profiles: [profile()] });
    render(<ProfilesPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Delete Slack" }));

    await waitFor(() => expect(invokeCalls("delete_app_profile")).toHaveLength(1));
    expect(invokeCalls("delete_app_profile")[0]).toEqual({ id: "profile-slack" });
    expect(await screen.findByText(/No rules yet/)).toBeTruthy();
  });

  it("shows the capture toggles off by default and saves the web-address one", async () => {
    mockWorld();
    render(<ProfilesPage />);

    const url = await screen.findByRole("switch", { name: "Read the web address" });
    expect(url.getAttribute("aria-checked")).toBe("false");
    expect(
      screen.getByRole("switch", { name: "Read the window title" }).getAttribute("aria-checked"),
    ).toBe("false");

    await userEvent.click(url);

    await waitFor(() =>
      expect(
        invokeCalls("set_setting").some(
          (args) =>
            (args as { key: string; value: string }).key === "profiles.capture_url" &&
            (args as { key: string; value: string }).value === "true",
        ),
      ).toBe(true),
    );
  });

  it("says what reading the web address costs, and that it is never stored", async () => {
    mockWorld();
    render(<ProfilesPage />);

    expect(
      await screen.findByText(
        /page address .* through Accessibility.*some browsers.*never stored on a rule/s,
      ),
    ).toBeTruthy();
  });

  it("shows a saved capture setting as on", async () => {
    mockWorld({ settings: { "profiles.capture_url": "true" } });
    render(<ProfilesPage />);

    await waitFor(() =>
      expect(
        screen
          .getByRole("switch", { name: "Read the web address" })
          .getAttribute("aria-checked"),
      ).toBe("true"),
    );
  });

  it("reports a failed load instead of showing an empty list", async () => {
    mockWorld({
      extra: {
        list_app_profiles: () => {
          throw new Error("ipc channel closed");
        },
      },
    });
    render(<ProfilesPage />);

    expect(await screen.findByText(/ipc channel closed/)).toBeTruthy();
  });
});
