import { describe, expect, it } from "vitest";
import {
  cloudEngineFor,
  credentialRefFor,
  ENGINE_LIST,
  ENGINES,
  engineSpec,
  enginesFor,
  needsDownload,
  runsLocally,
  type EngineId,
} from "./engines";

/**
 * The engine table is the one place an engine id is spelled in the UI, and the
 * ids have to match what `EngineRegistry` registers in Rust — an entry whose id
 * is wrong is an engine the picker silently never offers, and an id with no
 * entry is a binding nothing can describe.
 */
describe("engine identities", () => {
  it("keys every spec by its own id", () => {
    Object.entries(ENGINES).forEach(([key, spec]) => expect(spec.id).toBe(key));
  });

  it("registers the cloud engines the backend added, each against its provider", () => {
    const cases: [EngineId, string, string][] = [
      ["anthropic", "llm", "anthropic"],
      ["deepgram-stt", "stt", "deepgram"],
      ["elevenlabs-stt", "stt", "elevenlabs"],
    ];
    cases.forEach(([id, capability, providerRef]) => {
      const spec = engineSpec(id);
      expect(spec, id).toBeDefined();
      expect(spec!.capability).toBe(capability);
      expect(spec!.runsLocally).toBe(false);
      expect(spec!.credentialRef).toBe(providerRef);
      // A speech engine reaches the picker as one fixed row; a text engine is
      // offered once per connected provider instead, so only the first kind
      // carries a `cloudOption` (see buildCapabilityOptions).
      if (capability !== "llm") expect(spec!.cloudOption).toBeDefined();
    });
  });

  /**
   * Groq is deliberately absent: it serves an OpenAI-shaped
   * `/audio/transcriptions` and `/chat/completions`, so it is reached by
   * binding an existing engine to the `groq` provider_ref. An engine id here
   * would be one the backend never registers.
   */
  it("has no engine of its own for Groq, which rides the OpenAI-shaped ones", () => {
    expect(engineSpec("groq")).toBeUndefined();
    expect(engineSpec("groq-stt")).toBeUndefined();
    expect(cloudEngineFor("stt", "groq")).toBe("openai-stt");
    expect(cloudEngineFor("llm", "groq")).toBe("openai-compatible");
  });

  it("sends a provider with a branded engine to that engine, not the generic one", () => {
    expect(cloudEngineFor("llm", "anthropic")).toBe("anthropic");
    expect(cloudEngineFor("llm", "openai")).toBe("openai");
    expect(cloudEngineFor("stt", "deepgram")).toBe("deepgram-stt");
    expect(cloudEngineFor("stt", "elevenlabs")).toBe("elevenlabs-stt");
  });

  it("names the provider whose key each new cloud binding needs", () => {
    expect(
      credentialRefFor({ engine_id: "anthropic", model: null, provider_ref: null }),
    ).toBe("anthropic");
    expect(
      credentialRefFor({ engine_id: "deepgram-stt", model: "nova-3", provider_ref: null }),
    ).toBe("deepgram");
    expect(
      credentialRefFor({ engine_id: "elevenlabs-stt", model: null, provider_ref: null }),
    ).toBe("elevenlabs");
  });

  /**
   * Apple's recognizer is the second local engine with no catalog. The two
   * questions must be answered differently for it: it runs here, and there is
   * nothing to install — a slot diagnosed against "is that model on disk?"
   * would report a missing download for a binding that works.
   */
  it("treats Apple speech as local with nothing to download", () => {
    const spec = engineSpec("apple-speech");
    expect(spec).toBeDefined();
    expect(spec!.capability).toBe("stt");
    expect(runsLocally("apple-speech")).toBe(true);
    expect(needsDownload("apple-speech")).toBe(false);
    expect(spec!.catalog).toBeUndefined();
    expect(spec!.localOption).toBeDefined();
    // Never a key: a local engine must not be diagnosed as needing one.
    expect(
      credentialRefFor({ engine_id: "apple-speech", model: null, provider_ref: null }),
    ).toBeNull();
  });

  it("asks for a download only from the engines that have a catalog", () => {
    expect(needsDownload("whisper")).toBe(true);
    expect(needsDownload("parakeet")).toBe(true);
    expect(needsDownload("system-tts")).toBe(false);
    expect(needsDownload("openai-stt")).toBe(false);
    // An id from a build that does not know this engine is not a download.
    expect(needsDownload("nonesuch")).toBe(false);
  });

  /** The catalog now holds Moonshine as well, so the heading cannot say Parakeet only. */
  it("names the shared offline catalog for everything in it", () => {
    const title = engineSpec("parakeet")!.catalog!.title;
    expect(title).toContain("Moonshine");
    expect(title).toContain("Parakeet");
  });

  it("gives every engine a way to be offered and described", () => {
    ENGINE_LIST.forEach((spec) => {
      expect(typeof spec.describe, spec.id).toBe("function");
      // The four shapes buildCapabilityOptions can build a row from. An engine
      // matching none of them is registered in Rust and bindable nowhere.
      const offerable =
        spec.catalog ??
        spec.localOption ??
        spec.cloudOption ??
        (spec.capability === "llm" ? spec.credentialRef : undefined);
      expect(offerable, `${spec.id} can never appear in a picker`).toBeDefined();
    });
  });

  it("still has an any-provider engine to fall back to in every capability", () => {
    // `cloudEngineFor` lands on it for a provider no branded engine claims,
    // so a capability without one would have picks it could not resolve.
    (["llm", "stt", "tts"] as const).forEach((capability) => {
      expect(enginesFor(capability).length).toBeGreaterThan(0);
      expect(cloudEngineFor(capability, "made-up")).toBeTruthy();
    });
  });
});
