import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  emitTauriEvent,
  invokeCalls,
  onInvoke,
  resetTauriMocks,
} from "../test-utils/tauri";
import PromptPalette, { deliveryForKey } from "./PromptPalette";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

type Session = {
  session_id: number;
  source_text: string;
  origin: "selection" | "screen_capture";
  app_name: string | null;
  can_replace: boolean;
  can_insert: boolean;
  default_delivery: "replace" | "insert" | "copy";
  notice: string | null;
};

const SELECTION: Session = {
  session_id: 7,
  source_text: "we should probaly ship this on friday",
  origin: "selection",
  app_name: "TextEdit",
  can_replace: true,
  can_insert: true,
  default_delivery: "replace",
  notice: null,
};

function mockBackend(session: Session, history: string[] = []) {
  onInvoke({
    get_palette_session: () => session,
    list_palette_history: () => history,
    palette_ready: () => undefined,
    run_palette: () => ({ delivered: session.default_delivery, message: null }),
    cancel_palette: () => undefined,
  });
}

/** Mounts the palette and walks it through one open. */
async function openPalette(session: Session, history: string[] = []) {
  mockBackend(session, history);
  const view = render(<PromptPalette />);
  // Listeners are registered in an effect that awaits a promise.
  await act(async () => {});
  await act(async () => {
    emitTauriEvent("palette:open", { session_id: session.session_id });
  });
  // The open handler awaits three commands before the window would be shown.
  await act(async () => {});
  return view;
}

const input = () => screen.getByLabelText("Instruction") as HTMLInputElement;

async function type(value: string) {
  const field = input();
  await act(async () => {
    field.focus();
    const setter = Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!;
    setter.call(field, value);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function press(
  key: string,
  modifiers: { metaKey?: boolean; shiftKey?: boolean } = {},
) {
  await act(async () => {
    input().dispatchEvent(
      new KeyboardEvent("keydown", { key, bubbles: true, ...modifiers }),
    );
  });
}

describe("PromptPalette", () => {
  beforeEach(() => {
    resetTauriMocks();
  });

  it("renders nothing until a session arrives", async () => {
    mockBackend(SELECTION);
    render(<PromptPalette />);
    await act(async () => {});
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("previews the selection and tells the backend it is ready", async () => {
    await openPalette(SELECTION);
    expect(screen.getByLabelText("Selected text").textContent).toContain(
      "probaly ship this on friday",
    );
    // This is what puts the window on screen; without it the palette stays
    // hidden forever.
    expect(invokeCalls("palette_ready")).toEqual([{ sessionId: 7 }]);
  });

  it("shows the ask-anything state when nothing was selected", async () => {
    await openPalette({
      ...SELECTION,
      source_text: "",
      can_replace: false,
      default_delivery: "insert",
    });
    expect(screen.queryByLabelText("Selected text")).toBeNull();
    expect(screen.getByRole("dialog").textContent).toContain("Ask KEA anything");
    // With nothing to replace, Return has to mean something else.
    expect(screen.getByRole("dialog").textContent).toContain("Insert at the cursor");
  });

  it("badges a screen capture and shows its notice", async () => {
    await openPalette({
      ...SELECTION,
      source_text: "",
      origin: "screen_capture",
      can_replace: false,
      default_delivery: "copy",
      notice: "No text found in that capture.",
    });
    const dialog = screen.getByRole("dialog");
    expect(dialog.textContent).toContain("From screen capture");
    expect(dialog.textContent).toContain("No text found in that capture.");
    // An OCR result has no selection behind it, so Replace is never offered.
    expect(dialog.textContent).not.toContain("Replace the selection");
    // Return copies; inserting into whatever was behind the crosshair is an
    // option, not the default.
    expect(dialog.textContent).toContain("↩ Copy to the clipboard");
    expect(dialog.textContent).toContain("⌘↩ Insert at the cursor");
  });

  it.each([
    ["Enter", {}, "replace"],
    ["Enter", { metaKey: true }, "insert"],
    ["Enter", { metaKey: true, shiftKey: true }, "copy"],
  ] as const)(
    "%s%o submits as %s",
    async (key, modifiers, delivery) => {
      await openPalette(SELECTION);
      await type("make it shorter");
      await press(key, modifiers);

      expect(invokeCalls("run_palette")).toEqual([
        { sessionId: 7, instruction: "make it shorter", delivery },
      ]);
    },
  );

  it("ignores a second Return while the first is still running", async () => {
    // A request in flight is not a reason to spend a second provider call,
    // and the window is still up because the answer has not arrived.
    await openPalette(SELECTION);
    await type("make it shorter");
    await press("Enter");
    await press("Enter");
    expect(invokeCalls("run_palette").length).toBe(1);
  });

  it("ignores a Return with no instruction", async () => {
    await openPalette(SELECTION);
    await press("Enter");
    expect(invokeCalls("run_palette")).toEqual([]);
  });

  it("cancels on Escape without running anything", async () => {
    await openPalette(SELECTION);
    await type("make it shorter");
    await press("Escape");

    expect(invokeCalls("cancel_palette").length).toBe(1);
    expect(invokeCalls("run_palette")).toEqual([]);
    // The window is hidden by the backend; the component clears itself so the
    // next open cannot flash the previous session.
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("clears itself when the backend closes the palette", async () => {
    await openPalette(SELECTION);
    await act(async () => {
      emitTauriEvent("palette:close", null);
    });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("walks the instruction history and gives the draft back", async () => {
    await openPalette(SELECTION, ["make it shorter", "fix the grammar"]);
    await type("half-typed");

    await press("ArrowUp");
    expect(input().value).toBe("make it shorter");
    await press("ArrowUp");
    expect(input().value).toBe("fix the grammar");
    // Past the end stays put rather than wrapping to the draft.
    await press("ArrowUp");
    expect(input().value).toBe("fix the grammar");

    await press("ArrowDown");
    expect(input().value).toBe("make it shorter");
    await press("ArrowDown");
    // Losing a half-typed instruction to a stray arrow key is the bug this
    // component would otherwise ship with.
    expect(input().value).toBe("half-typed");
  });

  it("does nothing on arrow keys with no history", async () => {
    await openPalette(SELECTION, []);
    await type("half-typed");
    await press("ArrowUp");
    expect(input().value).toBe("half-typed");
  });

  it("keeps the instruction when a run fails", async () => {
    onInvoke({
      get_palette_session: () => SELECTION,
      list_palette_history: () => [],
      palette_ready: () => undefined,
      run_palette: () => {
        throw new Error("no llm engine available");
      },
    });
    render(<PromptPalette />);
    await act(async () => {});
    await act(async () => {
      emitTauriEvent("palette:open", { session_id: 7 });
    });
    await act(async () => {});

    await type("make it shorter");
    await press("Enter");

    expect(screen.getByRole("alert").textContent).toContain("no llm engine available");
    // Worth one edit and a second Return, not a retype.
    expect(input().value).toBe("make it shorter");
    expect(input().disabled).toBe(false);
  });
});

describe("deliveryForKey", () => {
  const both = { can_insert: true, default_delivery: "replace" } as const;

  it("maps the three submit combos", () => {
    const base = { key: "Enter", metaKey: false, ctrlKey: false, shiftKey: false };
    expect(deliveryForKey(base, both)).toBe("replace");
    expect(deliveryForKey({ ...base, metaKey: true }, both)).toBe("insert");
    expect(deliveryForKey({ ...base, metaKey: true, shiftKey: true }, both)).toBe("copy");
    // Ctrl stands in for Cmd off macOS, where the accelerator is
    // CommandOrControl.
    expect(deliveryForKey({ ...base, ctrlKey: true }, both)).toBe("insert");
  });

  it("is not a submit at all for other keys", () => {
    expect(
      deliveryForKey(
        { key: "a", metaKey: false, ctrlKey: false, shiftKey: false },
        both,
      ),
    ).toBeNull();
  });

  it("collapses insert into copy when KEA cannot type", () => {
    // Offering a key that silently does nothing is worse than offering fewer.
    const copyOnly = { can_insert: false, default_delivery: "copy" } as const;
    const base = { key: "Enter", metaKey: true, ctrlKey: false, shiftKey: false };
    expect(deliveryForKey(base, copyOnly)).toBe("copy");
    expect(deliveryForKey({ ...base, metaKey: false }, copyOnly)).toBe("copy");
  });
});
