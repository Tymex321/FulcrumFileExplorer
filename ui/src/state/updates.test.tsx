/**
 * The in-app update flow: the check, the sidebar card, and what happens when the network
 * or the installer fails. The backend is a fake; the real updater is Rust.
 */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { setApi } from "../api";
import { __resetMock, mockApi } from "../api/mock";
import type { FulcrumApi } from "../api/index";
import type { UpdateInfo } from "../api/types";
import { Sidebar } from "../components/Chrome";
import { app, checkForUpdate, dismissUpdate, installUpdate } from "./app";

const NEWER: UpdateInfo = { version: "1.0.2", current_version: "1.0.0", notes: null };

let root: Root;
let host: HTMLDivElement;

beforeAll(async () => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  Element.prototype.scrollIntoView ??= () => {};
  __resetMock();
  // The sidebar only ever mounts after startup has loaded settings.
  const settings = await mockApi.getSettings();
  app.set((s) => ({ ...s, settings }));
});

beforeEach(() => {
  app.set((s) => ({ ...s, update: null, updateBusy: "idle", updateDismissed: false, notices: [] }));
});

afterEach(() => {
  setApi(null);
});

afterAll(() => {
  host?.remove();
});

const withApi = (over: Partial<FulcrumApi>) => setApi({ ...mockApi, ...over });
const notices = () => app.get().notices.map((n) => n.text);
const text = () => host.textContent ?? "";
const button = (label: string) => [...host.querySelectorAll("button")].find((b) => b.textContent?.trim() === label);

const mountSidebar = async () => {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  await act(async () => root.render(<Sidebar />));
};

describe("checkForUpdate", () => {
  it("records a newer version", async () => {
    withApi({ checkForUpdate: async () => NEWER });
    await checkForUpdate(false);
    expect(app.get().update).toEqual(NEWER);
    expect(app.get().updateBusy).toBe("idle");
  });

  it("says nothing when the startup check finds nothing, but answers a manual check", async () => {
    withApi({ checkForUpdate: async () => null });
    await checkForUpdate(false);
    expect(notices()).toEqual([]);
    await checkForUpdate(true);
    expect(notices()).toEqual(["You're on the latest version."]);
  });

  it("stays silent about a failed startup check, and reports a failed manual one", async () => {
    withApi({
      checkForUpdate: async () => {
        throw { kind: "Other", detail: "offline" };
      },
    });
    await checkForUpdate(false);
    expect(notices()).toEqual([]);
    await checkForUpdate(true);
    expect(notices()[0]).toMatch(/Could not check for updates/);
    expect(app.get().updateBusy).toBe("idle");
  });

  it("ignores a second check while one is running", async () => {
    let calls = 0;
    withApi({
      checkForUpdate: async () => {
        calls++;
        await new Promise((r) => setTimeout(r, 20));
        return null;
      },
    });
    await Promise.all([checkForUpdate(false), checkForUpdate(false)]);
    expect(calls).toBe(1);
  });
});

describe("installUpdate", () => {
  it("shows the installing state while the installer runs", async () => {
    let release = () => {};
    withApi({ installUpdate: () => new Promise<void>((r) => (release = r)) });
    const done = installUpdate();
    expect(app.get().updateBusy).toBe("installing");
    await new Promise((r) => setTimeout(r, 0)); // let installUpdate reach the backend call
    release();
    await done;
    expect(app.get().updateBusy).toBe("installing"); // success means the app is about to exit
  });

  it("reports a failure, reverts to idle, and keeps the update offered", async () => {
    app.set((s) => ({ ...s, update: NEWER }));
    withApi({
      installUpdate: async () => {
        throw { kind: "Other", detail: "signature mismatch" };
      },
    });
    await installUpdate();
    expect(app.get().updateBusy).toBe("idle");
    expect(app.get().update).toEqual(NEWER);
    expect(notices()[0]).toMatch(/Update failed.*unchanged/);
  });
});

describe("update card", () => {
  it("is absent when there is no update", async () => {
    await mountSidebar();
    expect(text()).not.toContain("is available");
  });

  it("offers the update, installs on 'Update now', and hides on 'Later'", async () => {
    let installed = 0;
    withApi({
      installUpdate: async () => {
        installed++;
      },
    });
    app.set((s) => ({ ...s, update: NEWER }));
    await act(async () => {});
    expect(text()).toContain("Fulcrum 1.0.2 is available");
    expect(text()).toContain("You have 1.0.0");

    await act(async () => button("Later")?.click());
    expect(text()).not.toContain("is available");

    app.set((s) => ({ ...s, updateDismissed: false }));
    await act(async () => {});
    await act(async () => button("Update now")?.click());
    expect(installed).toBe(1);
  });

  it("stays visible while installing even if it was dismissed", async () => {
    app.set((s) => ({ ...s, update: NEWER, updateBusy: "installing" }));
    dismissUpdate();
    await act(async () => {});
    expect(text()).toContain("Installing 1.0.2");
    act(() => root.unmount());
  });
});
