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
import type { SearchHit } from "../api/types";
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
});
