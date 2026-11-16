/**
 * Guards the design system's promises by parsing styles.css itself, so a palette tweak
 * that breaks contrast fails here rather than in front of a user on a cheap panel.
 */
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { ROW_HEIGHTS } from "./tokens";

const css = readFileSync(resolve(dirname(fileURLToPath(import.meta.url)), "../styles.css"), "utf8");
const theme = /@theme \{([\s\S]*?)\n\}/.exec(css)?.[1] ?? "";

function token(name: string, source = theme): string {
  const m = new RegExp(`--${name}:\\s*(#[0-9a-fA-F]{6})`).exec(source);
  if (!m) throw new Error(`token --${name} not found`);
  return m[1]!;
}

function luminance(hex: string): number {
  const [r, g, b] = [1, 3, 5].map((i) => {
    const v = parseInt(hex.slice(i, i + 2), 16) / 255;
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!;
}

function contrast(a: string, b: string): number {
  const [x, y] = [luminance(a), luminance(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

const SURFACES = ["base", "surface", "raised", "overlay", "row-hover", "select-inactive"];

describe("design tokens", () => {
  it("keeps every text step at WCAG AA on every surface, including the lightest", () => {
    for (const text of ["ink", "ink-muted", "ink-faint"]) {
      for (const surface of SURFACES) {
        const c = contrast(token(`color-${text}`), token(`color-${surface}`));
        expect(c, `${text} on ${surface} = ${c.toFixed(2)}`).toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  it("keeps primary and muted text at AA on selected rows", () => {
    for (const bg of ["select", "select-hover"]) {
      for (const text of ["ink", "ink-muted"]) {
        expect(contrast(token(`color-${text}`), token(`color-${bg}`))).toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  it("keeps the focus ring visible (3:1 non-text) on every surface and on a selection", () => {
    for (const bg of [...SURFACES, "select", "select-hover", "drop"]) {
      const c = contrast(token("color-focus"), token(`color-${bg}`));
      expect(c, `focus on ${bg} = ${c.toFixed(2)}`).toBeGreaterThanOrEqual(3);
    }
  });

  it("makes hover and selection distinguishable from each other and from the list", () => {
    const base = token("color-base");
    const hover = token("color-row-hover");
    const select = token("color-select");
    expect(contrast(hover, base)).toBeGreaterThan(1.08);
    expect(contrast(select, hover)).toBeGreaterThan(1.15);
    expect(contrast(token("color-select-hover"), select)).toBeGreaterThan(1.05);
  });

  it("holds the same guarantees for every alternative accent", () => {
    for (const accent of ["Violet", "Magenta", "Indigo"]) {
      const block = new RegExp(`html\\[data-accent="${accent}"\\] \\{([\\s\\S]*?)\\}`).exec(css)?.[1] ?? "";
      for (const text of ["ink", "ink-muted"]) {
        expect(contrast(token(`color-${text}`), token("color-select", block))).toBeGreaterThanOrEqual(4.5);
      }
      expect(contrast(token("color-focus", block), token("color-select", block))).toBeGreaterThanOrEqual(3);
    }
  });

  it("mirrors the row heights the virtual list computes with", () => {
    for (const [density, px] of Object.entries(ROW_HEIGHTS)) {
      const m = new RegExp(`--row-${density.toLowerCase()}:\\s*(\\d+)px`).exec(css);
      expect(Number(m?.[1]), density).toBe(px);
    }
  });

  it("never animates slower than 200ms", () => {
    for (const m of css.matchAll(/--dur[\w-]*:\s*(\d+)ms/g)) expect(Number(m[1])).toBeLessThanOrEqual(200);
  });
});
