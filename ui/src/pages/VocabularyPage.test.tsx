import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import type { VocabularyEntry } from "../api";
import VocabularyPage from "./VocabularyPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

const KITTYCLAW: VocabularyEntry = {
  id: "vocab-1",
  term: "KittyClaw",
  sounds_like: "kitty claw, kitty paw",
  enabled: true,
  created_at: "2026-09-19T10:00:00Z",
};

const KEA: VocabularyEntry = {
  id: "vocab-2",
  term: "KEA",
  sounds_like: null,
  enabled: false,
  created_at: "2026-09-19T10:01:00Z",
};

type Handlers = Parameters<typeof onInvoke>[0];

/**
 * A live store rather than a fixed list: the page reloads after writing, so a
 * static `list_vocabulary` would let an add "succeed" without the new row ever
 * having to come back through the list.
 */
function mockStore(seed: VocabularyEntry[] = [], overrides: Handlers = {}) {
  const rows = [...seed];
  onInvoke({
    list_vocabulary: () => [...rows],
    upsert_vocabulary_entry: (args) => {
      const entry = (args as { entry: VocabularyEntry }).entry;
      const at = rows.findIndex((row) => row.id === entry.id);
      if (at >= 0) rows[at] = entry;
      else rows.push(entry);
    },
    delete_vocabulary_entry: (args) => {
      const id = (args as { id: string }).id;
      const at = rows.findIndex((row) => row.id === id);
      if (at >= 0) rows.splice(at, 1);
    },
    // Stands in for apply_vocabulary: enough to show the box wired up.
    preview_vocabulary: (args) =>
      (args as { text: string }).text.replace(/kitty claw/gi, "KittyClaw"),
    ...overrides,
  });
  return rows;
}

describe("VocabularyPage", () => {
  beforeEach(() => resetTauriMocks());

  it("lists the stored terms with their sounds-like and enabled state", async () => {
    mockStore([KITTYCLAW, KEA]);
    render(<VocabularyPage />);

    expect(await screen.findByText("KittyClaw")).toBeTruthy();
    expect(screen.getByText("kitty claw, kitty paw")).toBeTruthy();
    // A term with no sounds_like still gets a cell, not a blank one.
    expect(screen.getByText("KEA")).toBeTruthy();
    expect(screen.getByText("—")).toBeTruthy();

    expect(
      screen.getByRole("switch", { name: "Use KittyClaw" }).getAttribute("aria-checked"),
    ).toBe("true");
    expect(
      screen.getByRole("switch", { name: "Use KEA" }).getAttribute("aria-checked"),
    ).toBe("false");
  });

  it("explains the feature when nothing is stored", async () => {
    mockStore([]);
    render(<VocabularyPage />);

    expect(await screen.findByText(/KEA will spell it your way/)).toBeTruthy();
    expect(screen.queryByRole("table")).toBeNull();
  });

  it("adds a term with its sounds-like and shows it in the table", async () => {
    mockStore([]);
    render(<VocabularyPage />);

    await screen.findByText(/No terms yet/);
    await userEvent.type(screen.getByRole("textbox", { name: "Term" }), "KittyClaw");
    await userEvent.type(
      screen.getByRole("textbox", { name: "Sounds like" }),
      "kitty claw",
    );
    await userEvent.click(screen.getByRole("button", { name: "Add term" }));

    await waitFor(() => expect(invokeCalls("upsert_vocabulary_entry")).toHaveLength(1));
    expect(invokeCalls("upsert_vocabulary_entry")[0]).toEqual({
      entry: {
        id: expect.any(String),
        term: "KittyClaw",
        sounds_like: "kitty claw",
        enabled: true,
        created_at: expect.any(String),
      },
    });
    expect(await screen.findByRole("cell", { name: "KittyClaw" })).toBeTruthy();
    // The form empties so the next term is typed into a clean box.
    expect((screen.getByRole("textbox", { name: "Term" }) as HTMLInputElement).value).toBe("");
  });

  it("refuses a case-variant duplicate before the unique index does", async () => {
    mockStore([KITTYCLAW]);
    render(<VocabularyPage />);

    await screen.findByText("KittyClaw");
    await userEvent.type(screen.getByRole("textbox", { name: "Term" }), "kittyclaw");
    await userEvent.click(screen.getByRole("button", { name: "Add term" }));

    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      '"kittyclaw" is already in your vocabulary.',
    );
    expect(invokeCalls("upsert_vocabulary_entry")).toHaveLength(0);
  });

  it("deletes a term", async () => {
    mockStore([KITTYCLAW, KEA]);
    render(<VocabularyPage />);

    await userEvent.click(await screen.findByRole("button", { name: "Delete KittyClaw" }));

    await waitFor(() => expect(invokeCalls("delete_vocabulary_entry")).toHaveLength(1));
    expect(invokeCalls("delete_vocabulary_entry")[0]).toEqual({ id: "vocab-1" });
    await waitFor(() => expect(screen.queryByText("KittyClaw")).toBeNull());
    expect(screen.getByText("KEA")).toBeTruthy();
  });

  it("persists the enabled toggle", async () => {
    mockStore([KITTYCLAW]);
    render(<VocabularyPage />);

    await userEvent.click(await screen.findByRole("switch", { name: "Use KittyClaw" }));

    await waitFor(() => expect(invokeCalls("upsert_vocabulary_entry")).toHaveLength(1));
    expect(invokeCalls("upsert_vocabulary_entry")[0]).toEqual({
      entry: { ...KITTYCLAW, enabled: false },
    });
    expect(
      screen.getByRole("switch", { name: "Use KittyClaw" }).getAttribute("aria-checked"),
    ).toBe("false");
  });

  it("puts the toggle back when the write is refused", async () => {
    mockStore([KITTYCLAW], {
      upsert_vocabulary_entry: () => {
        throw "database is locked";
      },
    });
    render(<VocabularyPage />);

    await userEvent.click(await screen.findByRole("switch", { name: "Use KittyClaw" }));

    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "database is locked");
    expect(
      screen.getByRole("switch", { name: "Use KittyClaw" }).getAttribute("aria-checked"),
    ).toBe("true");
  });

  it("imports a list and reports what it added and skipped", async () => {
    mockStore([KITTYCLAW]);
    render(<VocabularyPage />);

    await screen.findByText("KittyClaw");
    await userEvent.type(
      screen.getByRole("textbox", { name: "Terms to import" }),
      // "kittyclaw" duplicates the stored row case-insensitively; "kea" repeats
      // inside the block itself.
      "kittyclaw{enter}KEA{enter}kea{enter}Parakeet",
    );
    await userEvent.click(screen.getByRole("button", { name: "Import terms" }));

    expect(
      await screen.findByText("Added 2 terms, skipped 2 duplicates."),
    ).toBeTruthy();
    expect(invokeCalls("upsert_vocabulary_entry").map((args) => {
      const { entry } = args as unknown as { entry: VocabularyEntry };
      return entry.term;
    })).toEqual(["KEA", "Parakeet"]);
    expect(await screen.findByRole("cell", { name: "Parakeet" })).toBeTruthy();
  });

  it("previews the vocabulary pass on typed text, debounced", async () => {
    mockStore([KITTYCLAW]);
    render(<VocabularyPage />);

    await screen.findByText("KittyClaw");
    await userEvent.type(
      screen.getByRole("textbox", { name: "Test sentence" }),
      "open kitty claw",
    );

    expect(await screen.findByText("open KittyClaw")).toBeTruthy();
    // One request for the whole phrase, not one per keystroke.
    expect(invokeCalls("preview_vocabulary")).toHaveLength(1);
    expect(invokeCalls("preview_vocabulary")[0]).toEqual({ text: "open kitty claw" });
  });

  it("says so when the vocabulary changes nothing", async () => {
    mockStore([KITTYCLAW]);
    render(<VocabularyPage />);

    await screen.findByText("KittyClaw");
    await userEvent.type(
      screen.getByRole("textbox", { name: "Test sentence" }),
      "nothing to fix here",
    );

    expect(await screen.findByText(/No changes/)).toBeTruthy();
  });

  it("reports a failed load instead of an empty vocabulary", async () => {
    mockStore([], {
      list_vocabulary: () => {
        throw "ipc channel closed";
      },
    });
    render(<VocabularyPage />);

    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "ipc channel closed");
  });
});
