import { describe, expect, it } from "vitest";
import type { FsOpRequest, Progress } from "../api/types";
import { detailLine, etaText, fraction, jobTitle } from "./jobs";

const req = (kind: FsOpRequest["kind"], sources = ["C:\\a\\x.txt"], destination: string | null = "D:\\Backups"): FsOpRequest => ({
  kind,
  sources,
  destination,
  conflict: "Ask",
  verify: false,
});

const prog = (patch: Partial<Progress>): Progress => ({
  op: 1,
  kind: "Copy",
  state: "Running",
  total_bytes: 1000,
  done_bytes: 250,
  total_items: 4,
  done_items: 1,
  current: "",
  bytes_per_sec: 100,
  eta_secs: 7,
  errors: [],
  pending_conflict: null,
  ...patch,
});

describe("progress honesty", () => {
  it("shows no fraction while scanning", () => {
    expect(fraction(prog({ state: "Scanning" }))).toBeNull();
    expect(fraction(null)).toBeNull();
  });

  it("weights by bytes, falling back to items", () => {
    expect(fraction(prog({}))).toBe(0.25);
    expect(fraction(prog({ total_bytes: 0, done_bytes: 0 }))).toBe(0.25);
    expect(fraction(prog({ state: "Done" }))).toBe(1);
  });

  it("says estimating rather than inventing an ETA", () => {
    expect(etaText(null)).toBe("estimating…");
    expect(detailLine(prog({ eta_secs: null }))).toContain("estimating…");
    expect(detailLine(prog({}))).toContain("7s left");
  });

  it("describes the scanning phase as counting", () => {
    expect(detailLine(prog({ state: "Scanning", total_items: 1200 }))).toMatch(/^Counting 1\D?200 items/);
  });

  it("explains a pause caused by a conflict", () => {
    const c = { source: "a", destination: "b", source_size: 1, dest_size: 2, source_modified: 0, dest_modified: 0 };
    expect(detailLine(prog({ state: "Paused", pending_conflict: c }))).toBe("Waiting for your decision");
  });
});

describe("jobTitle", () => {
  it("names one item, counts many, and names the destination", () => {
    expect(jobTitle(req("Copy"), null)).toBe("Copying “x.txt” to Backups");
    expect(jobTitle(req("Move", ["a", "b", "c"]), prog({ state: "Done" }))).toBe("Moved 3 items to Backups");
    expect(jobTitle(req("Trash", ["C:\\a"], null), null)).toBe("Moving to Recycle Bin “a”");
    expect(jobTitle(req("Copy"), prog({ state: "Cancelled" }))).toMatch(/^Cancelled/);
  });
});
