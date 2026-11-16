import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { setApi } from "../api";
import { __resetMock, mockApi } from "../api/mock";
import type { ListOptions } from "../api/types";
import { filtersFor, EMPTY_SEARCH, queryFor, TabRuntime } from "./tabRuntime";

const OPTS: ListOptions = {
  show_hidden: false,
  show_system: false,
  sort: { key: "Name", desc: false, dirs_first: true },
  compute_dir_sizes: false,
};

const until = async (cond: () => boolean, ms = 4000) => {
  const start = Date.now();
  while (!cond()) {
    if (Date.now() - start > ms) throw new Error("timed out");
    await new Promise((r) => setTimeout(r, 5));
  }
};

beforeEach(() => {
  __resetMock();
  setApi(mockApi);
});
afterEach(() => setApi(null));

describe("TabRuntime listing", () => {
  it("streams a 120k directory without copying it into React state", async () => {
    const rt = new TabRuntime();
    const seen: number[] = [];
    let done = false;
    const sample = () => {
      seen.push(rt.count);
      if (!done) setTimeout(sample, 1);
    };
    sample();
    await rt.load({ path: "D:\\Stress\\120k files", options: OPTS });
    done = true;
    const s = rt.getSnapshot();
    expect(s.loading).toBe(false);
    expect(s.count).toBe(rt.count);
    expect(s.count).toBeGreaterThan(110_000);
    // Painted progressively: some notification happened with a partial count.
    expect(seen.some((n) => n > 0 && n < s.count)).toBe(true);
    // Folders first, from the backend's sort.
    expect(rt.entryAt(0)?.kind).toBe("Dir");
    expect(rt.pathAt(0)).toMatch(/^D:\\Stress\\120k files\\/);
  });

  it("drops batches from a superseded navigation", async () => {
    const rt = new TabRuntime();
    const slow = rt.load({ path: "D:\\Stress\\120k files", options: OPTS });
    await rt.load({ path: "C:\\Users", options: OPTS });
    await slow;
    expect(rt.getSnapshot().count).toBe(2); // alex, Public (Default is hidden)
    expect(rt.current?.path).toBe("C:\\Users");
  });

  it("surfaces backend errors instead of an empty folder", async () => {
    const rt = new TabRuntime();
    await rt.load({ path: "Q:\\nope", options: OPTS });
    expect(rt.getSnapshot().error?.kind).toBe("NotFound");
  });

  it("keeps the selection by name across a refresh", async () => {
    const rt = new TabRuntime();
    await rt.load({ path: "C:\\Users\\alex", options: OPTS });
    const docs = Array.from({ length: rt.count }, (_, i) => rt.entryAt(i)!.name).indexOf("Documents");
    rt.setSelection(rt.getSnapshot().selection.select(docs));
    await rt.refresh();
    expect(rt.selectedEntries().map((e) => e.name)).toEqual(["Documents"]);
  });

  it("selects and starts renaming a name once it appears", async () => {
    const rt = new TabRuntime();
    await rt.load({ path: "C:\\Users\\alex", options: OPTS });
    await mockApi.createDir("C:\\Users\\alex\\New folder");
    rt.selectByName(["New folder"], { rename: true });
    await rt.refresh();
    const s = rt.getSnapshot();
    expect(s.renaming).not.toBeNull();
    expect(rt.entryAt(s.renaming!)?.name).toBe("New folder");
  });

  it("type-ahead jumps to a matching row", async () => {
    const rt = new TabRuntime();
    await rt.load({ path: "C:\\Users\\alex", options: OPTS });
    expect(rt.typeAhead("m")).toBe(true);
    expect(rt.entryAt(rt.getSnapshot().selection.focus!)?.name).toBe("Music");
  });
});

describe("TabRuntime search", () => {
  it("switches to search mode, streams hits with ranges, and back", async () => {
    const rt = new TabRuntime();
    await rt.load({ path: "D:\\Stress\\unicode", options: OPTS });
    rt.setSearch({ text: "café" }, true);
    await until(() => rt.getSnapshot().searchSummary !== null);
    const s = rt.getSnapshot();
    expect(s.mode).toBe("search");
    expect(s.count).toBe(1);
    expect(rt.hitAt(0)?.match_ranges.length).toBe(2);
    expect(s.searchSummary?.degraded).toBe(true);
    rt.setSearch({ text: "" });
    expect(rt.getSnapshot().mode).toBe("list");
    expect(rt.getSnapshot().count).toBe(6);
  });

  it("navigating ends a search", async () => {
    const rt = new TabRuntime();
    await rt.load({ path: "D:\\Stress\\unicode", options: OPTS });
    rt.setSearch({ text: "a" }, true);
    await until(() => rt.getSnapshot().searchSummary !== null);
    await rt.load({ path: "C:\\Users", options: OPTS });
    expect(rt.getSnapshot().mode).toBe("list");
    expect(rt.getSnapshot().search.text).toBe("");
  });
});

describe("filter chips", () => {
  it("map onto Rust Filters, with empty meaning unconstrained", () => {
    const f = filtersFor(EMPTY_SEARCH, false);
    expect(f.kinds).toEqual([]);
    expect(f.extensions).toEqual([]);
    expect(f.size).toBeNull();
    const g = filtersFor({ ...EMPTY_SEARCH, kind: "files", type: "images", size: "huge", date: "week" }, true, 1_000_000_000);
    expect(g.kinds).toEqual(["File"]);
    expect(g.extensions).toContain("png");
    expect(g.size?.min).toBe(1024 ** 3);
    expect(g.modified?.from).toBe(1_000_000_000 - 7 * 86_400_000);
    expect(g.include_hidden).toBe(true);
  });

  it("scopes to the current folder or the whole machine", () => {
    const sort = { key: "Name" as const, desc: false, dirs_first: true };
    expect(queryFor({ ...EMPTY_SEARCH, text: "x" }, "C:\\a", sort, false, 0, 10).scope).toEqual({ Subtree: "C:\\a" });
    expect(queryFor({ ...EMPTY_SEARCH, text: "x", scope: "everywhere" }, "C:\\a", sort, false, 0, 10).scope).toBe("All");
    expect(queryFor({ ...EMPTY_SEARCH, text: "x", mode: "Fuzzy" }, "C:\\a", sort, false, 0, 10).sort.key).toBe("Relevance");
  });
});
