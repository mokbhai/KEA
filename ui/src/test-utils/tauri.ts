import { vi } from "vitest";

/**
 * Shared Tauri mock for page/component tests. Wire it up per test file with:
 *
 *   vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
 *   vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);
 *
 * then route commands with `onInvoke({...})`, fire backend events with
 * `emitTauriEvent(...)`, and call `resetTauriMocks()` in beforeEach.
 */

type InvokeArgs = Record<string, unknown> | undefined;
type EventListener = (event: { payload: unknown }) => void;

export const invokeMock = vi.fn<(cmd: string, args?: InvokeArgs) => unknown>();

const listeners = new Map<string, Set<EventListener>>();

export const coreModule = {
  invoke: async (cmd: string, args?: InvokeArgs) => invokeMock(cmd, args),
};

export const eventModule = {
  listen: (event: string, handler: EventListener) => {
    let set = listeners.get(event);
    if (!set) {
      set = new Set();
      listeners.set(event, set);
    }
    set.add(handler);
    return Promise.resolve(() => {
      set.delete(handler);
    });
  },
};

/** Fires every listener registered for a Tauri event. */
export function emitTauriEvent(event: string, payload: unknown) {
  listeners.get(event)?.forEach((handler) => handler({ payload }));
}

/**
 * Engine registries a normal build has. Options are filtered against these,
 * so a test that omitted them would see an empty picker for the wrong reason.
 * Override per test to model a build without the local engines.
 */
export const DEFAULT_ENGINES: Record<string, { id: string; models: string[] }[]> = {
  list_llm_engines: [
    { id: "openai", models: [] },
    { id: "openai-compatible", models: [] },
  ],
  list_stt_engines: [
    { id: "whisper", models: [] },
    { id: "parakeet", models: [] },
    { id: "openai-stt", models: [] },
  ],
  list_tts_engines: [
    { id: "sherpa-tts", models: [] },
    { id: "openai-tts", models: [] },
  ],
};

/** Routes invoke calls to per-command handlers; unmocked commands reject. */
export function onInvoke(handlers: Record<string, (args?: InvokeArgs) => unknown>) {
  const withEngines: Record<string, (args?: InvokeArgs) => unknown> = {
    ...Object.fromEntries(
      Object.entries(DEFAULT_ENGINES).map(([cmd, list]) => [cmd, () => list]),
    ),
    ...handlers,
  };
  invokeMock.mockImplementation((cmd, args) => {
    const handler = withEngines[cmd];
    if (!handler) throw new Error(`unmocked invoke: ${cmd}`);
    return handler(args);
  });
}

export function resetTauriMocks() {
  invokeMock.mockReset();
  listeners.clear();
}

/** All calls to a given command, as their raw args objects. */
export function invokeCalls(cmd: string): InvokeArgs[] {
  return invokeMock.mock.calls.filter(([name]) => name === cmd).map(([, args]) => args);
}
