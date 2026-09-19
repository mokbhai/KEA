import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { LlmRate, UsageReport, UsageSpend } from "../api";
import { invokeCalls, onInvoke, resetTauriMocks } from "../test-utils/tauri";
import UsagePage from "./UsagePage";

vi.mock("@tauri-apps/api/core", async () => (await import("../test-utils/tauri")).coreModule);
vi.mock("@tauri-apps/api/event", async () => (await import("../test-utils/tauri")).eventModule);

const row = (over: Partial<UsageSpend> = {}): UsageSpend => ({
  feature_id: "rewrite",
  engine_id: "openai",
  model: "gpt-4o-mini",
  provider_ref: null,
  provider_key: "openai",
  calls: 4,
  unreported_calls: 0,
  prompt_tokens: 1_000_000,
  completion_tokens: 500_000,
  cost: null,
  currency: null,
  rate_updated_at: null,
  ...over,
});

const report = (over: Partial<UsageReport> = {}): UsageReport => ({
  totals: [row()],
  daily: [{ day: "2026-09-19", calls: 4, prompt_tokens: 1_000_000, completion_tokens: 500_000 }],
  any_rates: false,
  days: 30,
  ...over,
});

const mount = (usage: UsageReport, rates: LlmRate[] = []) => {
  onInvoke({
    get_usage_report: () => usage,
    list_llm_rates: () => rates,
    upsert_llm_rate: () => undefined,
    delete_llm_rate: () => undefined,
    clear_usage: () => 0,
  });
  render(<UsagePage />);
};

describe("UsagePage", () => {
  beforeEach(() => resetTauriMocks());

  it("shows tokens even when nothing can be priced", async () => {
    mount(report());

    expect(await screen.findByRole("heading", { level: 1, name: "Usage" })).toBeTruthy();
    // 1,000,000 + 500,000, as the locale formats it — in the summary and in
    // the row it came from.
    expect(await screen.findAllByText("1,500,000")).not.toHaveLength(0);
    expect(screen.getByText(/KEA ships no price list/i)).toBeTruthy();
  });

  it("shows a cost and the date of the rate it used", async () => {
    mount(
      report({
        any_rates: true,
        totals: [row({ cost: 0.45, currency: "USD", rate_updated_at: "2026-09-01 12:00:00" })],
      }),
    );

    // Once in the summary, once in the row it came from.
    expect(await screen.findAllByText("$0.45")).toHaveLength(2);
    // A rate is only as good as its date, so the date travels with the money.
    expect(screen.getByText(/rate of/).textContent).toContain("2026-09-01");
  });

  it("says why a group with an unreported call is not priced", async () => {
    // The tokens are a floor, so a price computed from them would be a floor
    // printed as a total.
    mount(report({ any_rates: true, totals: [row({ calls: 4, unreported_calls: 1 })] }));

    expect(await screen.findByText(/1 of 4 calls reported nothing/i)).toBeTruthy();
    expect(
      screen.getByText(/totals above are a floor rather than a total/i),
    ).toBeTruthy();
  });

  it("calls a provider that never reports counts what it is", async () => {
    mount(report({ totals: [row({ calls: 3, unreported_calls: 3, model: null })] }));

    expect(
      await screen.findByText("This provider does not report token counts."),
    ).toBeTruthy();
  });

  it("re-reads the report when the window changes", async () => {
    mount(report());
    await waitFor(() => expect(invokeCalls("get_usage_report")).toHaveLength(1));

    await userEvent.click(screen.getByRole("button", { name: "7 days" }));

    await waitFor(() => expect(invokeCalls("get_usage_report")).toHaveLength(2));
    expect(invokeCalls("get_usage_report")[1]).toEqual({ days: 7 });
  });

  it("saves a rate without sending a date of its own", async () => {
    mount(report());
    await screen.findByRole("heading", { level: 1, name: "Usage" });

    // The last one is the blank form under Rates; the per-row buttons above
    // prefill themselves from the row they sit in.
    const add = await screen.findAllByRole("button", { name: "Add a rate" });
    await userEvent.click(add[add.length - 1]);
    await userEvent.type(screen.getByLabelText("Rate provider"), "openai");
    await userEvent.type(screen.getByLabelText("Rate model"), "gpt-4o-mini");
    await userEvent.click(screen.getByRole("button", { name: "Save rate" }));

    await waitFor(() => expect(invokeCalls("upsert_llm_rate")).toHaveLength(1));
    const sent = invokeCalls("upsert_llm_rate")[0]?.rate as LlmRate;
    expect(sent.provider_key).toBe("openai");
    // The backend stamps the date. Sending one back would let an untouched
    // rate keep looking freshly checked.
    expect(sent.updated_at).toBe("");
  });

  it("offers nothing to read when there were no calls", async () => {
    mount(report({ totals: [], daily: [] }));

    expect(await screen.findByText(/No AI calls in the last 30 days/i)).toBeTruthy();
  });
});
