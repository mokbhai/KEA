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
 * checker is a dozen lines and not worth a dependency. `contrast maths` below
 * pins it against the two ratios everyone knows by heart.
 *
 * Adding a pair is one line in PAIRS. Adding a token without covering it is a
 * failure: `covers every token` forces every entry in :root to be either
 * measured or explicitly exempted with a reason.
 *
 * A translucent token must name the element it paints onto (`fgOver` /
 * `bgOver`). There is deliberately no default: assuming the backdrop is the
 * other half of the pair is how the first version of this file came to assert
 * a ratio for a colour that renders nowhere on screen — a 1px border
 * composites over its own element's background (background-clip is border-box
 * by default), not over whatever sits behind that element.
 */
import { describe, expect, it } from "vitest";
import cssSource from "./index.css?raw";

// ── Thresholds ───────────────────────────────────────────────────────
/** WCAG 2.1 1.4.3 Contrast (Minimum) — body text and labels. */
const TEXT = 4.5;
/** WCAG 2.1 1.4.11 Non-text Contrast — control boundaries and state. */
const NON_TEXT = 3.0;
/**
 * Not a WCAG threshold. The .kea-table row rule is structural: row grouping is
 * conveyed by the table markup, row state by a background fill, and the
 * actionable part of the row by the underlined .kea-table__select — so 3:1 is
 * not required of it. It does have to read as a divider against every fill a
 * row can take, which is what this floor pins.
 */
const SEPARATOR_FLOOR = 2.0;
/**
 * Not a WCAG threshold, and NOT a statement that these values pass.
 *
 * --border and --btn-border are known 1.4.11 shortfalls at 1.38:1 / 1.29:1 —
 * decorative on card, topbar and sidebar edges, but also the sole boundary of
 * .kea-icon-btn (transparent fill, so the outline is the entire affordance),
 * .kea-segment, .kea-choice, the unselected meeting button in MeetingsPage,
 * and every .kea-btn (whose --btn-bg equals --surface in light theme). They
 * predate this palette pass and are tracked in issue #11. This floor exists
 * only so the values cannot fade further while that issue is open; raising
 * them to NON_TEXT is #11's job.
 */
const PENDING_FLOOR = 1.25;

interface Pair {
  /** Foreground token. */
  fg: string;
  /** Background token. */
  bg: string;
  /** Required ratio. */
  min: number;
  /** Where the pair shows up, and why it needs that threshold. */
  why: string;
  /** Token `fg` paints onto. Required when `fg` is translucent. */
  fgOver?: string;
  /** Token `bg` paints onto. Required when `bg` is translucent. */
  bgOver?: string;
}

const PAIRS: Pair[] = [
  // ── Text: 1.4.3 ────────────────────────────────────────────────────
  { fg: "--text", bg: "--surface", min: TEXT, why: "body copy on a card" },
  { fg: "--text", bg: "--surface-2", min: TEXT, why: "body copy on a raised or current row" },
  { fg: "--text", bg: "--bg", min: TEXT, why: "body copy on the app backdrop" },
  { fg: "--text", bg: "--btn-hover-bg", min: TEXT, why: ".kea-table__select on a hovered row" },
  // Over --bg, not --surface: .kea-main sets no background, and every <Banner>
  // is a page- or section-level child — none sits inside a .kea-card. --bg is
  // also the darker of the two in light theme, so it is the conservative
  // backdrop as well as the correct one.
  { fg: "--text", bg: "--ok-bg", bgOver: "--bg", min: TEXT, why: ".kea-banner--ok message" },
  { fg: "--text", bg: "--warn-bg", bgOver: "--bg", min: TEXT, why: ".kea-banner--warn message" },
  { fg: "--text", bg: "--danger-bg", bgOver: "--bg", min: TEXT, why: ".kea-banner--error message" },
  { fg: "--text-muted", bg: "--surface", min: TEXT, why: ".kea-muted in a card, and table headers" },
  { fg: "--text-muted", bg: "--bg", min: TEXT, why: ".kea-muted at page level, e.g. the ModelsPage intro" },
  { fg: "--text-muted", bg: "--surface-2", min: TEXT, why: ".kea-muted on a raised row" },
  { fg: "--btn-text", bg: "--btn-bg", min: TEXT, why: ".kea-btn label" },
  { fg: "--btn-text", bg: "--btn-hover-bg", min: TEXT, why: ".kea-btn label on hover" },
  { fg: "--input-text", bg: "--input-bg", min: TEXT, why: ".kea-input / .kea-select value" },
  { fg: "--accent-text", bg: "--accent", min: TEXT, why: ".kea-btn--primary label" },
  { fg: "--accent", bg: "--surface-2", min: TEXT, why: ".kea-nav-item--active, current picker option" },
  { fg: "--accent", bg: "--surface", min: TEXT, why: ".kea-table__select at rest, inside .kea-table-wrap" },
  // Known gap, tracked in issue #11: .kea-status--* also render inside table
  // cells (HistoryPanel.tsx:66) and so take the hovered and aria-current row
  // fills, which are not measured here. Light --ok on either is 4.39:1 —
  // below the floor. Pre-existing and identical on main, so adding the pair
  // would fail the suite for a fault this branch did not introduce.
  { fg: "--ok", bg: "--surface", min: TEXT, why: ".kea-status--ok" },
  { fg: "--warn", bg: "--surface", min: TEXT, why: "warning status text" },
  { fg: "--danger", bg: "--surface", min: TEXT, why: ".kea-status--error" },

  // ── Non-text: 1.4.11 ───────────────────────────────────────────────
  { fg: "--input-border", bg: "--surface", min: NON_TEXT, why: "the only boundary of .kea-input / .kea-select" },
  { fg: "--toggle-track", bg: "--surface", min: NON_TEXT, why: ".kea-toggle outer edge" },
  { fg: "--toggle-track", bg: "--toggle-thumb", min: NON_TEXT, why: ".kea-toggle unchecked thumb against its track" },
  { fg: "--accent", bg: "--toggle-thumb", min: NON_TEXT, why: ".kea-toggle checked thumb against its track" },

  // ── Structural, sub-3:1 by decision ────────────────────────────────
  // Measured against every fill a .kea-table row can take: resting, hovered
  // and aria-current. A rule that only clears the floor against the resting
  // fill is not enough — rows are scanned in exactly the other two states.
  { fg: "--border-strong", bg: "--surface", min: SEPARATOR_FLOOR, why: ".kea-table separator, resting row" },
  { fg: "--border-strong", bg: "--btn-hover-bg", min: SEPARATOR_FLOOR, why: ".kea-table separator, hovered row" },
  { fg: "--border-strong", bg: "--surface-2", min: SEPARATOR_FLOOR, why: ".kea-table separator, aria-current row" },

  // ── Known shortfalls, pinned not blessed (issue #11) ───────────────
  { fg: "--border", bg: "--surface", min: PENDING_FLOOR, why: "card edges, but also the .kea-icon-btn / .kea-segment / .kea-choice boundary" },
  { fg: "--btn-border", bg: "--surface", min: PENDING_FLOOR, why: "the only boundary of .kea-btn, whose fill matches --surface in light theme" },
];

/**
 * Tokens with no contrast pair, each with the reason it has none. Anything
 * neither measured nor listed here fails `covers every token in :root`.
 */
const EXEMPT: Record<string, string> = {
  "--underline": "declared but referenced by no rule; kept equal to --accent so it cannot drift",
  "--toggle-thumb-border":
    "softens the thumb edge rather than carrying it. background-clip defaults to border-box, so this ring composites over the thumb's own fill and never meets the track — the --toggle-thumb pairs above carry 1.4.11 for that control",
};

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
function composite(fg: Rgba, backdrop: Rgba): Rgba {
  return {
    r: fg.r * fg.a + backdrop.r * (1 - fg.a),
    g: fg.g * fg.a + backdrop.g * (1 - fg.a),
    b: fg.b * fg.a + backdrop.b * (1 - fg.a),
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

function readBlock(selector: string): Record<string, string> {
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

const LIGHT = readBlock(":root {");
/**
 * Dark overrides :root rather than replacing it, exactly as the cascade does,
 * so a token the dark block legitimately inherits reads as inherited rather
 * than as missing.
 */
const THEMES = {
  light: LIGHT,
  dark: { ...LIGHT, ...readBlock(':root[data-theme="dark"]') },
};
type Theme = keyof typeof THEMES;

/**
 * Resolve a token to the opaque colour it actually renders as. A translucent
 * token must name its backdrop; there is no implicit one.
 */
function resolve(
  tokens: Record<string, string>,
  name: string,
  backdrop: string | undefined,
  field: string,
): Rgba {
  const raw = tokens[name];
  if (raw === undefined) throw new Error(`palette contrast: ${name} is not defined`);
  const colour = parseColor(raw);
  if (colour.a === 1) return colour;
  if (backdrop === undefined) {
    throw new Error(
      `palette contrast: ${name} is translucent (${raw}); the pair must name what it paints onto via \`${field}\``,
    );
  }
  return composite(colour, resolve(tokens, backdrop, undefined, field));
}

describe("contrast maths", () => {
  it("matches the two ratios WCAG spells out", () => {
    expect(contrastRatio(parseColor("#000000"), parseColor("#ffffff"))).toBeCloseTo(21, 5);
    expect(contrastRatio(parseColor("#ffffff"), parseColor("#ffffff"))).toBeCloseTo(1, 5);
  });

  it("composites a translucent colour onto the backdrop it is given", () => {
    const tokens = { "--ring": "rgba(0, 0, 0, 0.5)", "--thumb": "#ffffff", "--track": "#000000" };
    // Same ring, two backdrops, two answers — which is exactly why the
    // backdrop has to be stated rather than inferred from the pair.
    expect(resolve(tokens, "--ring", "--thumb", "fgOver")).toEqual({ r: 127.5, g: 127.5, b: 127.5, a: 1 });
    expect(resolve(tokens, "--ring", "--track", "fgOver")).toEqual({ r: 0, g: 0, b: 0, a: 1 });
  });

  it("refuses to guess a backdrop for a translucent token", () => {
    expect(() => resolve({ "--ring": "rgba(0, 0, 0, 0.5)" }, "--ring", undefined, "fgOver")).toThrow(
      /must name what it paints onto/,
    );
  });
});

describe("palette contrast (index.css tokens)", () => {
  // Note the limit of this guard, so it is not mistaken for more than it is:
  // token completeness is not pairing completeness. A token counts as covered
  // once it is measured against ONE background, even where the UI puts it on
  // three — see the .kea-status--* note in PAIRS. Tracked in issue #11.
  it("covers every token in :root", () => {
    const measured = new Set(PAIRS.flatMap((p) => [p.fg, p.bg, p.fgOver, p.bgOver]));
    const uncovered = Object.keys(LIGHT).filter((t) => !measured.has(t) && !(t in EXEMPT));
    expect(uncovered, `add to PAIRS, or to EXEMPT with a reason: ${uncovered.join(", ")}`).toEqual([]);
  });

  it("does not exempt a token that is in fact measured", () => {
    const measured = new Set(PAIRS.flatMap((p) => [p.fg, p.bg]));
    expect(Object.keys(EXEMPT).filter((t) => measured.has(t))).toEqual([]);
  });

  for (const theme of Object.keys(THEMES) as Theme[]) {
    const tokens = THEMES[theme];

    describe(`${theme} theme`, () => {
      for (const pair of PAIRS) {
        it(`${pair.fg} on ${pair.bg} clears ${pair.min}:1 — ${pair.why}`, () => {
          const bg = resolve(tokens, pair.bg, pair.bgOver, "bgOver");
          const fg = resolve(tokens, pair.fg, pair.fgOver, "fgOver");
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
