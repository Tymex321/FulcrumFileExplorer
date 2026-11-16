/**
 * End-to-end smoke test: boots the whole app against the mock backend in jsdom and
 * drives it purely through keyboard chords, the way a user would. It proves the
 * keybinding registry is actually wired to behaviour, not just internally consistent.
 */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { setApi } from "./api";
import { __resetMock, mockApi } from "./api/mock";
import { App } from "./App";

// jsdom has no layout: give scroll containers a real-looking size so rows render.
beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
  Element.prototype.scrollIntoView ??= () => {};
  Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => 600 });
  Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => 900 });
  localStorage.clear();
  __resetMock();
  setApi(mockApi);
});

let root: Root;
let host: HTMLDivElement;

afterAll(() => {
  act(() => root.unmount());
  host.remove();
  setApi(null);
});

const settle = async (ms = 60) => {
  await act(async () => {
    await new Promise((r) => setTimeout(r, ms));
  });
};

const press = async (key: string, mods: Partial<KeyboardEventInit> = {}) => {
  const target = document.activeElement ?? document.body;
  await act(async () => {
    target.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...mods }));
  });
  await settle(40);
};

const tabs = () => [...document.querySelectorAll("[role=tab]")].map((t) => t.textContent);
const rowNames = () => [...document.querySelectorAll(".list [data-index]")].map((r) => r.textContent ?? "");
const selected = () => [...document.querySelectorAll(".list [data-selected=true]")].map((r) => r.textContent ?? "");
const status = () => document.querySelector("section footer")?.textContent ?? "";

describe("App against the mock backend", () => {
  it("boots and lists the first drive", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    await act(async () => root.render(<App />));
    await settle(200);
    expect(tabs()).toEqual(["C:"]);
    expect(rowNames().some((n) => n.startsWith("Users"))).toBe(true);
    // Hidden and system entries stay hidden by default.
    expect(rowNames().some((n) => n.includes("pagefile.sys"))).toBe(false);
  });

  it("moves the cursor, opens a folder with Enter and goes back with Backspace", async () => {
    await press("End");
    expect(selected()[0]).toMatch(/^Users/);
    await press("Enter");
    await settle(100);
    expect(tabs()).toEqual(["Users"]);
    expect(rowNames().some((n) => n.startsWith("alex"))).toBe(true);
    await press("Backspace");
    await settle(100);
    expect(tabs()).toEqual(["C:"]);
  });

  it("selects all with Ctrl+A and reports it in the status bar", async () => {
    await press("a", { ctrlKey: true });
    expect(selected().length).toBe(rowNames().length);
    expect(status()).toMatch(/4\s*selected/);
    await press("Escape");
    expect(selected().length).toBe(0);
  });

  it("opens, closes and restores tabs from the keyboard", async () => {
    await press("t", { ctrlKey: true });
    expect(tabs().length).toBe(2);
    await press("w", { ctrlKey: true });
    expect(tabs().length).toBe(1);
    await press("T", { ctrlKey: true, shiftKey: true });
    expect(tabs().length).toBe(2);
    await press("w", { ctrlKey: true });
  });

  it("type-to-select jumps to a matching name", async () => {
    await press("p");
    expect(selected()[0]).toMatch(/^PerfLogs/);
    await press("p");
    expect(selected()[0]).toMatch(/^Program Files/);
  });

  it("splits the window and gives each pane its own tab strip", async () => {
    await press("\\", { ctrlKey: true, code: "Backslash" });
    await settle(100);
    expect(document.querySelectorAll("section[aria-label^=Pane]").length).toBe(2);
    await press("\\", { ctrlKey: true, code: "Backslash" });
    expect(document.querySelectorAll("section[aria-label^=Pane]").length).toBe(1);
  });

  it("opens the command palette and runs a command from it", async () => {
    await press("P", { ctrlKey: true, shiftKey: true });
    const input = document.querySelector<HTMLInputElement>("[aria-label='Palette query']");
    expect(input?.value).toBe(">");
    await act(async () => {
      const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      set.call(input, ">grid");
      input!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await settle(60);
    await press("Enter");
    await settle(60);
    expect(document.querySelector("[aria-label='Palette query']")).toBeNull();
    expect(document.querySelector("[role=radio][aria-label=Grid]")?.getAttribute("aria-checked")).toBe("true");
  });

  it("shows the instant-search affordance while the index is degraded", () => {
    expect(document.body.textContent).toContain("Instant search is off");
  });
});
