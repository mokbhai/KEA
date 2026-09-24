import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { onInvoke, resetTauriMocks } from "../test-utils/tauri";
import { featureHandlers, openAiBinding } from "../test-utils/featureWorld";
import AssistantPage from "./AssistantPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

describe("AssistantPage", () => {
  beforeEach(() => {
    resetTauriMocks();
    onInvoke(featureHandlers({ binding: openAiBinding() }));
  });

  it("shows the shortcut row", async () => {
    render(<AssistantPage />);
    expect(await screen.findByText("Shortcut")).toBeTruthy();
  });

  it("advertises Escape, which is otherwise undiscoverable", async () => {
    // Escape is registered only while a session is open, so a user who never
    // reads this row has no way to learn the key does anything.
    render(<AssistantPage />);
    expect(await screen.findByText("Esc")).toBeTruthy();
  });

  it("lists what the assistant can do", async () => {
    render(<AssistantPage />);
    await waitFor(() => {
      expect(screen.getByText("Open an application")).toBeTruthy();
    });
    expect(screen.getByText("Read what's on screen")).toBeTruthy();
    expect(screen.getByText("Rewrite the selection")).toBeTruthy();
  });

  it("says plainly that nothing it does can destroy work", async () => {
    // The reason there is no confirmation flow anywhere in this feature, and
    // the thing a user needs to know before letting it read their screen.
    render(<AssistantPage />);
    expect(
      await screen.findByText(/changes or deletes your work/i),
    ).toBeTruthy();
  });
});
