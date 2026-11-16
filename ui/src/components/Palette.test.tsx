/**
 * Regression test for the "Go to file" palette showing "No matches" while its footer
 * reported matches. Result batches travel on a separate channel from the search call's own
 * response, so they can land *after* the call resolves; the palette must still render them.
 */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { setApi } from "../api";
import { __resetMock, mockApi } from "../api/mock";
import type { Query, QuerySummary, SearchHit } from "../api/types";
import { openPalette } from "../state/app";
import { CommandPalette } from "./Overlays";

let root: Root;
let host: HTMLDivElement;

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  Element.prototype.scrollIntoView ??= () => {};
  localStorage.clear();
  __resetMock();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterAll(() => {
  act(() => root.unmount());
  host.remove();
  setApi(null);
});

const settle = async (ms: number) => {
  await act(async () => {
    await new Promise((r) => setTimeout(r, ms));
  });
};

/** Polls until `cond` holds (the mock backend simulates search latency), or gives up. */
const until = async (cond: () => boolean, timeoutMs = 3000) => {
  const start = Date.now();
  while (!cond() && Date.now() - start < timeoutMs) await settle(25);
};

const type = async (text: string) => {
  const input = document.querySelector<HTMLInputElement>("input[aria-label='Palette query']");
  if (!input) throw new Error("palette input not found");
  await act(async () => {
    // React tracks the value itself, so go through the native setter to trigger onChange.
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, text);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
};

const optionCount = () => document.querySelectorAll("[role=option]").length;
const body = () => document.querySelector("[role=dialog]")?.textContent ?? "";

describe("command palette file search", () => {
  it("shows results whose batch arrives after the search call has returned", async () => {
    setApi({
      ...mockApi,
      search: async (query, onBatch) => {
        const hits: SearchHit[] = [];
        const summary = await mockApi.search(query, (b) => hits.push(...b));
        // The response resolves first; the rows follow on the channel a moment later.
        setTimeout(() => onBatch(hits), 30);
        return summary;
      },
    });
    await act(async () => root.render(<CommandPalette />));
    await act(async () => openPalette("files"));

    await type("users");
    await until(() => optionCount() > 0);

    expect(optionCount()).toBeGreaterThan(0);
    expect(body()).not.toContain("No matches");
  });

  it("never shows a match count beside an empty list when the search fails", async () => {
    let fail = false;
    setApi({
      ...mockApi,
      search: async (query, onBatch) => {
        if (fail) throw new Error("backend unavailable");
        return mockApi.search(query, onBatch);
      },
    });
    await type("");
    await type("users");
    await until(() => /\d+ matches ·/.test(body()));
    expect(body()).toMatch(/\d+ matches ·/);

    fail = true;
    await type("userss");
    await until(() => !/\d+ matches ·/.test(body()));
    expect(optionCount()).toBe(0);
    expect(body()).not.toMatch(/\d+ matches ·/);
  });

  describe("accuracy", () => {
    const summary = (n: number): QuerySummary => ({ returned: n, total_matched: n, elapsed_micros: 1000, truncated: false, degraded: false });
    const named = (name: string, ranges: number[] = []): SearchHit => ({
      entry: { ...(mockApi_hit.entry), name },
      volume: "D",
      path: `D:\\Data\\${name}`,
      score: 1,
      match_ranges: ranges,
    });
    let mockApi_hit: SearchHit;
    let modes: string[] = [];

    /** A backend that answers Substring and Fuzzy queries from fixed lists. */
    const fake = (literal: SearchHit[], fuzzy: SearchHit[]) => {
      modes = [];
      setApi({
        ...mockApi,
        search: async (q: Query, onBatch) => {
          modes.push(q.mode);
          const list = q.mode === "Substring" ? literal : fuzzy;
          onBatch(list);
          return summary(list.length);
        },
      });
    };

    beforeAll(async () => {
      let first: SearchHit | undefined;
      await mockApi.search({ text: "users", mode: "Substring", case: "Smart", scope: "All", filters: { kinds: [], extensions: [], size: null, modified: null, created: null, include_hidden: false, include_system: false }, sort: { key: "Name", desc: false, dirs_first: false }, limit: 1, offset: 0, match_full_path: false }, (b) => {
        first ??= b[0];
      });
      if (!first) throw new Error("mock has no sample hit");
      mockApi_hit = first;
    });

    const names = () => [...document.querySelectorAll("[role=option]")].map((o) => o.textContent ?? "");

    it("shows only literal matches, best first, and never asks for fuzzy ones", async () => {
      fake([named("MissionModule_zadanie_Base.uasset", [14, 21]), named("zadanie.txt", [0, 7]), named("zadanie_final.docx", [0, 7])], [named("scattered_junk.dds", [1, 2, 4, 5, 8, 9])]);
      await type("");
      await type("zadanie");
      await until(() => names().length === 3);
      expect(names().map((n) => n.split("D:")[0])).toEqual(["zadanie.txt", "zadanie_final.docx", "MissionModule_zadanie_Base.uasset"]);
      expect(modes).not.toContain("Fuzzy");
      expect(body()).toMatch(/3 matches/);
    });

    it("falls back to fuzzy only when nothing matches literally, keeping close matches only", async () => {
      fake([], [named("zadanie.txt", [0, 3, 4, 7]), named("MissionModule_zSubmodule_AddSpecial.uasset", [8, 9, 15, 16, 20, 21, 24, 25, 28, 29, 31, 32])]);
      await type("zadnie");
      await until(() => /close matches/.test(body()));
      expect(names().length).toBe(1);
      expect(names()[0]).toContain("zadanie.txt");
      expect(body()).toMatch(/1 close matches/);
      expect(body()).toContain("nothing matched exactly");
      expect(modes).toEqual(["Substring", "Fuzzy"]);
    });

    it("does not fall back to fuzzy for very short queries", async () => {
      fake([], [named("zadanie.txt", [0, 1])]);
      await type("za");
      await until(() => modes.length > 0);
      await settle(120);
      expect(modes).toEqual(["Substring"]);
      expect(names().length).toBe(0);
    });
  });
});
