/**
 * Contrast guard for the design tokens in index.css.
 *
 * The tokens are the single source of truth: this file reads them straight out
 * of the stylesheet rather than keeping a second copy, so a palette edit that
 * drops a pair below its threshold fails here instead of in an audit months
 * later.
 *
 * The ratio maths is WCAG 2.1 verbatim (sRGB channel -> relative luminance ->
 * (L1 + 0.05) / (L2 + 0.05)) and is implemented locally on purpose — a contrast
 * checker is a dozen lines and not worth a dependency. `describes the maths`
 * below pins it against the two ratios everyone knows by heart.
 *
 * Adding a pair is one line in PAIRS.
 */
import { describe, expect, it } from "vitest";
import cssSource from "./index.css?raw";

// ── WCAG 2.1 thresholds ──────────────────────────────────────────────
/** 1.4.3 Contrast (Minimum) — body text and labels. */
const TEXT = 4.5;
/** 1.4.11 Non-text Contrast — control boundaries and state indicators. */
const NON_TEXT = 3.0;
/**
 * Not a WCAG threshold. Floors for the two purely structural rules, recorded
 * so nobody quietly fades them back to invisibility. See the notes on each
 * pair for why 3:1 is not required of them.
 */
const SEPARATOR_FLOOR = 2.0;
const DECORATIVE_FLOOR = 1.25;

type Theme = "light" | "dark";

interface Pair {
  /** Foreground token. If it is translucent it is composited over `bg`. */
  fg: string;
  /** Background token. Must be opaque. */
  bg: string;
  /** Required ratio. */
  min: number;
  /** Where the pair shows up, and why it needs that threshold. */
  why: string;
  /** Defaults to both themes. Set when the two themes solve it differently. */
  themes?: Theme[];
}

const PAIRS: Pair[] = [
  // ── Text: 1.4.3 ────────────────────────────────────────────────────
  { fg: "--text", bg: "--surface", min: TEXT, why: "body copy on a card" },
  { fg: "--text", bg: "--surface-2", min: TEXT, why: "body copy on a raised row" },
  { fg: "--text", bg: "--bg", min: TEXT, why: "body copy on the app backdrop" },
  { fg: "--text-muted", bg: "--surface", min: TEXT, why: ".kea-muted and table headers" },
  { fg: "--text-muted", bg: "--surface-2", min: TEXT, why: ".kea-muted on a raised row" },
  { fg: "--btn-text", bg: "--btn-bg", min: TEXT, why: ".kea-btn label" },
  { fg: "--btn-text", bg: "--btn-hover-bg", min: TEXT, why: ".kea-btn label on hover" },
  { fg: "--input-text", bg: "--input-bg", min: TEXT, why: ".kea-input / .kea-select value" },
  { fg: "--accent-text", bg: "--accent", min: TEXT, why: ".kea-btn--primary label" },
  { fg: "--accent", bg: "--surface-2", min: TEXT, why: ".kea-nav-item--active, current picker option" },
  { fg: "--accent", bg: "--surface", min: TEXT, why: ".kea-table__select and links" },

  // ── Non-text: 1.4.11 ───────────────────────────────────────────────
  { fg: "--input-border", bg: "--surface", min: NON_TEXT, why: "the only boundary of .kea-input / .kea-select" },
  { fg: "--toggle-track", bg: "--surface", min: NON_TEXT, why: ".kea-toggle outer edge" },
  { fg: "--toggle-track", bg: "--toggle-thumb", min: NON_TEXT, why: ".kea-toggle unchecked thumb against its track" },
  // Checked .kea-toggle: light theme separates thumb from track by fill, dark
  // theme cannot (white on --accent is 2.87:1) and separates them with the
  // 1px --toggle-thumb-border hairline instead.
  { fg: "--accent", bg: "--toggle-thumb", min: NON_TEXT, why: ".kea-toggle checked thumb against its track", themes: ["light"] },
  { fg: "--toggle-thumb-border", bg: "--accent", min: NON_TEXT, why: ".kea-toggle checked thumb hairline against its track", themes: ["dark"] },

  // ── Structural, sub-3:1 by decision ────────────────────────────────
  // .kea-table row rules. Not a 1.4.11 case: the row grouping is conveyed by
  // the table markup, row state by a background fill, and the actionable part
  // of the row by the underlined .kea-table__select — the rule identifies
  // nothing on its own. It does still have to read as a divider, which
  // --border (1.38 / 1.29) did not, hence the separate token.
  { fg: "--border-strong", bg: "--surface", min: SEPARATOR_FLOOR, why: ".kea-table row separator" },
  // Card / topbar / sidebar edges. Decorative; the surface fill already
  // distinguishes the container from the backdrop.
  { fg: "--border", bg: "--surface", min: DECORATIVE_FLOOR, why: "decorative container edge" },
];

// ── WCAG 2.1 contrast maths ──────────────────────────────────────────
interface Rgba {
  r: number;
  g: number;
  b: number;
  a: number;
}

function parseColor(value: string): Rgba {
  const hex = /^#([\da-f]{6})$/i.exec(value);
  if (hex) {
    const n = Number.parseInt(hex[1], 16);
    return { r: (n >> 16) & 0xff, g: (n >> 8) & 0xff, b: n & 0xff, a: 1 };
  }
  const fn = /^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*(?:,\s*([\d.]+)\s*)?\)$/i.exec(value);
  if (fn) {
    return {
      r: Number(fn[1]),
      g: Number(fn[2]),
      b: Number(fn[3]),
      a: fn[4] === undefined ? 1 : Number(fn[4]),
    };
  }
  throw new Error(`palette contrast: cannot read colour value ${JSON.stringify(value)}`);
}

/** Source-over composite of a translucent colour onto an opaque backdrop. */
function composite(fg: Rgba, bg: Rgba): Rgba {
  return {
    r: fg.r * fg.a + bg.r * (1 - fg.a),
    g: fg.g * fg.a + bg.g * (1 - fg.a),
    b: fg.b * fg.a + bg.b * (1 - fg.a),
    a: 1,
  };
}

/** WCAG 2.1: 8-bit sRGB channel to its linear-light value. */
function linearize(channel8: number): number {
  const c = channel8 / 255;
  return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

/** WCAG 2.1 relative luminance. */
function relativeLuminance({ r, g, b }: Rgba): number {
  return 0.2126 * linearize(r) + 0.7152 * linearize(g) + 0.0722 * linearize(b);
}

/** WCAG 2.1 contrast ratio, (L1 + 0.05) / (L2 + 0.05) with L1 the lighter. */
function contrastRatio(a: Rgba, b: Rgba): number {
  const la = relativeLuminance(a);
  const lb = relativeLuminance(b);
  const [lighter, darker] = la >= lb ? [la, lb] : [lb, la];
  return (lighter + 0.05) / (darker + 0.05);
}

// ── Token extraction ─────────────────────────────────────────────────
const WITHOUT_COMMENTS = cssSource.replace(/\/\*[\s\S]*?\*\//g, "");

function readTokens(selector: string): Record<string, string> {
  const start = WITHOUT_COMMENTS.indexOf(selector);
  if (start === -1) throw new Error(`palette contrast: no ${selector} block in index.css`);
  const open = WITHOUT_COMMENTS.indexOf("{", start);
  const close = WITHOUT_COMMENTS.indexOf("}", open);
  if (open === -1 || close === -1) throw new Error(`palette contrast: malformed ${selector} block`);
  const tokens: Record<string, string> = {};
  for (const [, name, value] of WITHOUT_COMMENTS.slice(open + 1, close).matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
    tokens[name] = value.trim();
  }
  return tokens;
}

const THEMES: Record<Theme, Record<string, string>> = {
  light: readTokens(":root {"),
  dark: readTokens(':root[data-theme="dark"]'),
};

describe("contrast maths", () => {
  it("matches the two ratios WCAG spells out", () => {
    const black = parseColor("#000000");
    const white = parseColor("#ffffff");
    expect(contrastRatio(black, white)).toBeCloseTo(21, 5);
    expect(contrastRatio(white, white)).toBeCloseTo(1, 5);
  });

  it("composites translucent foregrounds onto the backdrop", () => {
    const half = parseColor("rgba(0, 0, 0, 0.5)");
    expect(composite(half, parseColor("#ffffff"))).toEqual({ r: 127.5, g: 127.5, b: 127.5, a: 1 });
  });
});

describe("palette contrast (index.css tokens)", () => {
  for (const theme of Object.keys(THEMES) as Theme[]) {
    const tokens = THEMES[theme];

    describe(`${theme} theme`, () => {
      it("defines every token the pairs reference", () => {
        const missing = [...new Set(PAIRS.flatMap((p) => [p.fg, p.bg]))].filter((t) => !(t in tokens));
        expect(missing).toEqual([]);
      });

      for (const pair of PAIRS) {
        if (pair.themes && !pair.themes.includes(theme)) continue;

        it(`${pair.fg} on ${pair.bg} clears ${pair.min}:1 — ${pair.why}`, () => {
          const bg = parseColor(tokens[pair.bg]);
          const rawFg = parseColor(tokens[pair.fg]);
          const fg = rawFg.a < 1 ? composite(rawFg, bg) : rawFg;
          const ratio = contrastRatio(fg, bg);

          expect(
            ratio,
            `${theme} theme: ${pair.fg} (${tokens[pair.fg]}) on ${pair.bg} (${tokens[pair.bg]}) ` +
              `measured ${ratio.toFixed(2)}:1, needs ${pair.min.toFixed(2)}:1 — ${pair.why}`,
          ).toBeGreaterThanOrEqual(pair.min);
        });
      }
    });
  }
});
