import { describe, expect, it } from "vitest";
import {
  activeTab,
  back,
  canGoBack,
  canGoForward,
  closeTab,
  cycleTab,
  forward,
  fromSession,
  HISTORY_LIMIT,
  initialWorkspace,
  moveTab,
  moveTabToPane,
  navigate,
  openTab,
  restoreClosed,
  setLayout,
  toggleSort,
  toSession,
  type Workspace,
} from "./workspace";

const paths = (ws: Workspace, pane: 0 | 1 = 0) => ws.panes[pane].tabs.map((t) => t.path);

describe("tabs", () => {
  it("opens new tabs next to the active one", () => {
    let ws = initialWorkspace("C:\\A");
    ws = openTab(ws, 0, "C:\\B");
    ws = openTab(ws, 0, "C:\\C");
    ws = { ...ws, panes: [{ ...ws.panes[0], active: 0 }, ws.panes[1]] };
    ws = openTab(ws, 0, "C:\\D");
    expect(paths(ws)).toEqual(["C:\\A", "C:\\D", "C:\\B", "C:\\C"]);
    expect(activeTab(ws)?.path).toBe("C:\\D");
  });

  it("can open in the background without stealing focus", () => {
    let ws = initialWorkspace("C:\\A");
    ws = openTab(ws, 0, "C:\\B", { activate: false });
    expect(activeTab(ws)?.path).toBe("C:\\A");
  });

  it("closing the active tab activates its right neighbour", () => {
    let ws = initialWorkspace("C:\\A");
    ws = openTab(ws, 0, "C:\\B");
    ws = openTab(ws, 0, "C:\\C");
    ws = closeTab(ws, 0, 1); // close B (active is C at 2)
    expect(activeTab(ws)?.path).toBe("C:\\C");
    ws = closeTab(ws, 0, 1); // close C, the last one
    expect(activeTab(ws)?.path).toBe("C:\\A");
  });

  it("never closes the last tab of the window", () => {
    const ws = initialWorkspace("C:\\A");
    expect(closeTab(ws, 0, 0)).toBe(ws);
  });

  it("restores closed tabs in reverse order at their old position", () => {
    let ws = initialWorkspace("C:\\A");
    ws = openTab(ws, 0, "C:\\B");
    ws = openTab(ws, 0, "C:\\C");
    ws = closeTab(ws, 0, 1);
    ws = closeTab(ws, 0, 1);
    expect(paths(ws)).toEqual(["C:\\A"]);
    ws = restoreClosed(ws);
    expect(paths(ws)).toEqual(["C:\\A", "C:\\C"]);
    ws = restoreClosed(ws);
    expect(paths(ws)).toEqual(["C:\\A", "C:\\B", "C:\\C"]);
    expect(restoreClosed(ws)).toBe(ws);
  });

  it("reorders while keeping the active tab active", () => {
    let ws = initialWorkspace("C:\\A");
    ws = openTab(ws, 0, "C:\\B");
    ws = openTab(ws, 0, "C:\\C"); // active C
    ws = moveTab(ws, 0, 2, 0);
    expect(paths(ws)).toEqual(["C:\\C", "C:\\A", "C:\\B"]);
    expect(activeTab(ws)?.path).toBe("C:\\C");
  });

  it("cycles with wrap-around", () => {
    let ws = initialWorkspace("C:\\A");
    ws = openTab(ws, 0, "C:\\B");
    ws = cycleTab(ws, 1);
    expect(activeTab(ws)?.path).toBe("C:\\A");
    ws = cycleTab(ws, -1);
    expect(activeTab(ws)?.path).toBe("C:\\B");
  });
});

describe("history", () => {
  it("goes back and forward and discards forward history on navigate", () => {
    let ws = initialWorkspace("C:\\");
    ws = navigate(ws, 0, "C:\\Users");
    ws = navigate(ws, 0, "C:\\Users\\alex");
    ws = back(ws, 0);
    expect(activeTab(ws)?.path).toBe("C:\\Users");
    expect(canGoForward(activeTab(ws))).toBe(true);
    ws = navigate(ws, 0, "C:\\Windows");
    expect(canGoForward(activeTab(ws))).toBe(false);
    ws = back(ws, 0);
    ws = back(ws, 0);
    expect(activeTab(ws)?.path).toBe("C:\\");
    expect(canGoBack(activeTab(ws))).toBe(false);
    expect(back(ws, 0)).toEqual(ws);
    ws = forward(ws, 0);
    expect(activeTab(ws)?.path).toBe("C:\\Users");
  });

  it("does not record a navigation to the same folder", () => {
    let ws = initialWorkspace("C:\\Users");
    ws = navigate(ws, 0, "c:\\Users\\".replace("c", "C"));
    expect(activeTab(ws)?.history).toEqual(["C:\\Users"]);
  });

  it("caps history length", () => {
    let ws = initialWorkspace("C:\\");
    for (let i = 0; i < HISTORY_LIMIT + 20; i++) ws = navigate(ws, 0, `C:\\d${i}`);
    expect(activeTab(ws)?.history.length).toBe(HISTORY_LIMIT);
  });
});

describe("dual pane", () => {
  it("splitting seeds the second pane with the current folder", () => {
    const ws = setLayout(initialWorkspace("C:\\Users"), "Vertical");
    expect(paths(ws, 1)).toEqual(["C:\\Users"]);
  });

  it("panes keep independent history", () => {
    let ws = setLayout(initialWorkspace("C:\\"), "Vertical");
    ws = navigate(ws, 1, "D:\\");
    ws = navigate(ws, 0, "C:\\Users");
    expect(activeTab(ws, 1)?.history).toEqual(["C:\\", "D:\\"]);
    expect(activeTab(ws, 0)?.history).toEqual(["C:\\", "C:\\Users"]);
  });

  it("closing the last tab of a pane collapses the split onto the other pane", () => {
    let ws = setLayout(initialWorkspace("C:\\"), "Horizontal");
    ws = navigate(ws, 1, "D:\\");
    ws = closeTab(ws, 0, 0);
    expect(ws.layout).toBe("Single");
    expect(paths(ws)).toEqual(["D:\\"]);
  });

  it("unsplitting keeps the active pane on screen and remembers the other", () => {
    let ws = setLayout(initialWorkspace("C:\\"), "Vertical");
    ws = navigate(ws, 1, "D:\\");
    ws = { ...ws, activePane: 1 };
    ws = setLayout(ws, "Single");
    expect(paths(ws, 0)).toEqual(["D:\\"]);
    ws = setLayout(ws, "Vertical");
    expect(paths(ws, 1)).toEqual(["C:\\"]);
  });

  it("moves a tab across panes but never empties the source", () => {
    let ws = setLayout(initialWorkspace("C:\\A"), "Vertical");
    ws = openTab(ws, 0, "C:\\B");
    ws = moveTabToPane(ws, 0, 1, 1, 0);
    expect(paths(ws, 0)).toEqual(["C:\\A"]);
    expect(paths(ws, 1)).toEqual(["C:\\B", "C:\\A"]);
    expect(ws.activePane).toBe(1);
    expect(moveTabToPane(ws, 0, 0, 1, 0)).toBe(ws);
  });
});

describe("sorting", () => {
  it("flips direction on the same key and picks sensible defaults for a new key", () => {
    const s = { key: "Name" as const, desc: false, dirs_first: true };
    expect(toggleSort(s, "Name").desc).toBe(true);
    expect(toggleSort(s, "Size")).toEqual({ key: "Size", desc: true, dirs_first: true });
    expect(toggleSort(s, "Extension").desc).toBe(false);
  });
});

describe("session round-trip", () => {
  it("persists only the visible panes and restores them", () => {
    let ws = setLayout(initialWorkspace("C:\\"), "Vertical");
    ws = navigate(ws, 1, "D:\\Data");
    ws = openTab(ws, 0, "C:\\Users");
    const s = toSession(ws);
    expect(s.panes.length).toBe(2);
    expect(s.active_tab).toEqual([1, 0]);

    const back = fromSession(s, "C:\\");
    expect(back.layout).toBe("Vertical");
    expect(paths(back, 0)).toEqual(["C:\\", "C:\\Users"]);
    expect(activeTab(back, 1)?.history).toEqual(["C:\\", "D:\\Data"]);

    const single = toSession(initialWorkspace("C:\\"));
    expect(single.panes.length).toBe(1);
  });

  it("repairs a corrupt or partial session instead of failing", () => {
    expect(paths(fromSession(null, "C:\\"))).toEqual(["C:\\"]);
    const broken = {
      layout: "Diagonal",
      panes: [[{ path: "", view: "Details", sort: null, history: [], history_pos: 0 }]],
      active_pane: 7,
      active_tab: [99],
    } as never;
    const ws = fromSession(broken, "C:\\");
    expect(ws.layout).toBe("Single");
    expect(paths(ws)).toEqual(["C:\\"]);

    const outOfRange = fromSession(
      {
        layout: "Vertical",
        panes: [[{ path: "C:\\X", view: "Grid", sort: { key: "Size", desc: true, dirs_first: false }, history: ["C:\\X"], history_pos: 50 }], []],
        active_pane: 1,
        active_tab: [4, 0],
      },
      "C:\\",
    );
    expect(outOfRange.layout).toBe("Single"); // empty second pane cannot be split
    expect(activeTab(outOfRange)?.historyPos).toBe(0);
    expect(activeTab(outOfRange)?.view).toBe("Grid");
  });
});
