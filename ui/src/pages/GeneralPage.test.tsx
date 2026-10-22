import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { vi } from "vitest";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import { ThemeProvider } from "../theme";
import GeneralPage from "./GeneralPage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

const API_OFF = {
  enabled: false,
  running: false,
  socket_path: "/tmp/kea/kea-api.sock",
  token_file: "/tmp/kea/api-token",
  max_rewrites_per_minute: 20,
  supported: true,
  unix_socket_supported: true,
  tcp_enabled: false,
  tcp_running: false,
  tcp_error: null,
  tcp_port: 8765,
};

type Handlers = Record<string, (args?: Record<string, unknown>) => unknown>;

function mockGeneralWorld(overrides: Handlers = {}) {
  const settings = new Map<string, string>();
  onInvoke({
    get_setting: (args) => settings.get(args?.key as string) ?? null,
    set_setting: (args) => {
      settings.set(args?.key as string, args?.value as string);
    },
    get_autostart: () => false,
    set_autostart: () => undefined,
    get_all_permission_statuses: () => [],
    get_api_settings: () => API_OFF,
    set_api_enabled: (args) => ({
      ...API_OFF,
      enabled: args?.enabled as boolean,
      running: args?.enabled as boolean,
    }),
    set_api_rate_limit: () => undefined,
    reveal_api_token: () => "t0k3n",
    regenerate_api_token: () => "fr3sh",
    install_cli_shim: () => "/usr/local/bin/kea",
    ...overrides,
  });
}

function renderPage() {
  return render(
    <ThemeProvider>
      <GeneralPage onRunSetup={() => {}} />
    </ThemeProvider>,
  );
}

describe("GeneralPage", () => {
  beforeEach(() => resetTauriMocks());

  it("titles the page with the only h1", async () => {
    mockGeneralWorld();
    renderPage();

    expect(
      await screen.findByRole("heading", { level: 1, name: "General" }),
    ).toBeTruthy();
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
  });

  it("saves the appearance preference from the row", async () => {
    mockGeneralWorld();
    renderPage();

    const select = await screen.findByRole("combobox", { name: "Appearance" });
    await userEvent.selectOptions(select, "dark");

    expect(document.documentElement.dataset.theme).toBe("dark");
    await waitFor(() =>
      expect(
        invokeCalls("set_setting").some(
          (args) => args?.key === "ui.theme" && args?.value === "dark",
        ),
      ).toBe(true),
    );
    expect(screen.getByText("Saved ✓")).toBeTruthy();
  });

  it("toggles launch at login and persists it", async () => {
    mockGeneralWorld();
    renderPage();

    const toggle = await screen.findByRole("switch", { name: "Launch KEA at login" });
    expect(toggle.getAttribute("aria-checked")).toBe("false");

    await userEvent.click(toggle);

    await waitFor(() => expect(invokeCalls("set_autostart")).toHaveLength(1));
    expect(invokeCalls("set_autostart")[0]).toEqual({ enabled: true });
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    expect(screen.getByText("Saved ✓")).toBeTruthy();
  });

  it("defaults dictation sounds on and persists turning them off", async () => {
    mockGeneralWorld();
    renderPage();

    const toggle = await screen.findByRole("switch", { name: "Play dictation sounds" });
    expect(toggle.getAttribute("aria-checked")).toBe("true");

    await userEvent.click(toggle);

    await waitFor(() =>
      expect(
        invokeCalls("set_setting").some(
          (args) => args?.key === "sound.cues_enabled" && args?.value === "false",
        ),
      ).toBe(true),
    );
    expect(toggle.getAttribute("aria-checked")).toBe("false");
  });

  it("states the threat model before the local API can be turned on", async () => {
    mockGeneralWorld();
    renderPage();

    // The capability sentence, not a reassurance: this is what the user is
    // agreeing to when they flip the toggle.
    expect(
      await screen.findByText(/type text into whatever app you have focused/),
    ).toBeTruthy();
  });

  it("turns the local API on and reports what the backend says is listening", async () => {
    let current = API_OFF;
    mockGeneralWorld({
      get_api_settings: () => current,
      set_api_enabled: (args) => {
        current = {
          ...API_OFF,
          enabled: args?.enabled as boolean,
          running: args?.enabled as boolean,
        };
        return current;
      },
    });
    renderPage();

    const toggle = await screen.findByRole("switch", { name: "Enable the local API" });
    expect(toggle.getAttribute("aria-checked")).toBe("false");

    await userEvent.click(toggle);

    await waitFor(() => expect(invokeCalls("set_api_enabled")).toHaveLength(1));
    expect(invokeCalls("set_api_enabled")[0]).toEqual({ enabled: true });
    await waitFor(() =>
      expect(screen.getByText(/Listening on \/tmp\/kea\/kea-api\.sock/)).toBeTruthy(),
    );
  });

  it("leaves the toggle off when the socket refuses to bind", async () => {
    mockGeneralWorld({
      set_api_enabled: () => {
        throw new Error("could not bind /tmp/kea/kea-api.sock: Address already in use");
      },
    });
    renderPage();

    const toggle = await screen.findByRole("switch", { name: "Enable the local API" });
    await userEvent.click(toggle);

    // Rolled back rather than left claiming an API that is not there.
    await waitFor(() => expect(toggle.getAttribute("aria-checked")).toBe("false"));
    expect(screen.getByText(/Address already in use/)).toBeTruthy();
  });

  it("reveals the token and replaces it on regenerate", async () => {
    mockGeneralWorld();
    renderPage();

    await userEvent.click(await screen.findByRole("button", { name: "Reveal" }));
    expect(await screen.findByText("t0k3n")).toBeTruthy();

    await userEvent.click(screen.getByRole("button", { name: "Regenerate" }));
    expect(await screen.findByText("fr3sh")).toBeTruthy();
  });

  it("saves a new rate limit on blur and rejects a nonsense one", async () => {
    mockGeneralWorld();
    renderPage();

    const input = await screen.findByRole("spinbutton", { name: "Rewrites per minute" });
    await userEvent.clear(input);
    await userEvent.type(input, "5");
    await userEvent.tab();

    await waitFor(() => expect(invokeCalls("set_api_rate_limit")).toHaveLength(1));
    expect(invokeCalls("set_api_rate_limit")[0]).toEqual({ limit: 5 });

    await userEvent.clear(input);
    await userEvent.tab();
    // Nothing written, and the field goes back to the saved value.
    expect(invokeCalls("set_api_rate_limit")).toHaveLength(1);
    await waitFor(() => expect((input as HTMLInputElement).value).toBe("5"));
  });

  it("reports where the command-line tool landed", async () => {
    mockGeneralWorld();
    renderPage();

    await userEvent.click(await screen.findByRole("button", { name: "Install" }));
    expect(await screen.findByText(/Installed at \/usr\/local\/bin\/kea/)).toBeTruthy();
  });

  it("reflects sounds already turned off", async () => {
    const settings = new Map<string, string>([["sound.cues_enabled", "false"]]);
    mockGeneralWorld({
      get_setting: (args) => settings.get(args?.key as string) ?? null,
    });
    renderPage();

    await waitFor(() =>
      expect(
        screen
          .getByRole("switch", { name: "Play dictation sounds" })
          .getAttribute("aria-checked"),
      ).toBe("false"),
    );
  });

  it("shows the localhost-port warning when the port is turned on", async () => {
    let current = API_OFF;
    mockGeneralWorld({
      get_api_settings: () => current,
      set_api_tcp_enabled: (args) => {
        current = { ...current, tcp_enabled: args?.enabled as boolean, tcp_running: true };
        return current;
      },
    });
    renderPage();

    const toggle = await screen.findByRole("switch", { name: "Allow localhost port" });
    expect(screen.queryByText(/Holding the token lets a program/)).toBeNull();

    await userEvent.click(toggle);

    await waitFor(() => expect(invokeCalls("set_api_tcp_enabled")).toHaveLength(1));
    expect(invokeCalls("set_api_tcp_enabled")[0]).toEqual({ enabled: true });
    expect(await screen.findByText(/Holding the token lets a program/)).toBeTruthy();
    expect(screen.getByText(/Listening on 127\.0\.0\.1:8765/)).toBeTruthy();
  });

  it("shows the bind conflict inline and leaves the port off", async () => {
    mockGeneralWorld({
      set_api_tcp_enabled: () => {
        throw new Error("could not bind 127.0.0.1:8765: Address already in use");
      },
    });
    renderPage();

    const toggle = await screen.findByRole("switch", { name: "Allow localhost port" });
    await userEvent.click(toggle);

    await waitFor(() => expect(toggle.getAttribute("aria-checked")).toBe("false"));
    expect(await screen.findByText(/Address already in use/)).toBeTruthy();
    expect(screen.queryByText(/Holding the token lets a program/)).toBeNull();
  });

  it("saves a new port on blur and refuses an out-of-range one", async () => {
    mockGeneralWorld({
      set_api_tcp_port: (args) => ({ ...API_OFF, tcp_port: args?.port as number }),
    });
    renderPage();

    const input = await screen.findByRole("spinbutton", { name: "Localhost port" });
    await userEvent.clear(input);
    await userEvent.type(input, "9000");
    await userEvent.tab();

    await waitFor(() => expect(invokeCalls("set_api_tcp_port")).toHaveLength(1));
    expect(invokeCalls("set_api_tcp_port")[0]).toEqual({ port: 9000 });

    await userEvent.clear(input);
    await userEvent.type(input, "70000");
    await userEvent.tab();

    expect(invokeCalls("set_api_tcp_port")).toHaveLength(1);
    expect(await screen.findByText(/between 1 and 65535/)).toBeTruthy();
  });

  it("shows the port-in-use message when the new port cannot be bound", async () => {
    mockGeneralWorld({
      set_api_tcp_port: () => {
        throw new Error("port 9000 is already in use");
      },
    });
    renderPage();

    const input = await screen.findByRole("spinbutton", { name: "Localhost port" });
    await userEvent.clear(input);
    await userEvent.type(input, "9000");
    await userEvent.tab();

    expect(await screen.findByText(/port 9000 is already in use/)).toBeTruthy();
  });

  it("gives the base URL for the port when it is on, with a placeholder key", async () => {
    mockGeneralWorld({
      get_api_settings: () => ({ ...API_OFF, tcp_enabled: true, tcp_running: true }),
    });
    renderPage();

    await waitFor(() =>
      expect(document.body.textContent).toContain("Base URL: http://127.0.0.1:8765/v1"),
    );
    expect(document.body.textContent).toContain("<your KEA access token>");
    expect(document.body.textContent).not.toContain("curl --unix-socket");
  });

  it("shows the socket curl example when the port is off", async () => {
    mockGeneralWorld();
    renderPage();

    await waitFor(() =>
      expect(document.body.textContent).toContain(
        "curl --unix-socket /tmp/kea/kea-api.sock",
      ),
    );
    expect(document.body.textContent).toContain("temperature");
    expect(document.body.textContent).toContain("whole reply as a single chunk");
  });

  it("names the port as the only transport and hides the socket on Windows", async () => {
    mockGeneralWorld({
      get_api_settings: () => ({
        ...API_OFF,
        socket_path: "",
        unix_socket_supported: false,
      }),
    });
    renderPage();

    expect(
      await screen.findByText(/the localhost port is the only way to reach the API/),
    ).toBeTruthy();
    expect(document.body.textContent).not.toContain("curl --unix-socket");
    expect(document.body.textContent).not.toContain("kea-api.sock");
  });
});
