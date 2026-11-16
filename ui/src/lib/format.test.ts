import { describe, expect, it } from "vitest";
import { breadcrumbs, formatDate, formatEta, formatRate, formatSize } from "./format";

describe("formatSize", () => {
  it("uses base-1024 units", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(512)).toBe("512 B");
    expect(formatSize(1024)).toBe("1.00 KB");
    expect(formatSize(1536)).toBe("1.50 KB");
    expect(formatSize(1024 ** 3)).toBe("1.00 GB");
  });

  it("narrows decimals as the number grows, to keep the column stable", () => {
    expect(formatSize(1024 * 5)).toBe("5.00 KB");
    expect(formatSize(1024 * 50)).toBe("50.0 KB");
    expect(formatSize(1024 * 500)).toBe("500 KB");
  });

  it("does not invent a size for unknown or invalid input", () => {
    expect(formatSize(-1)).toBe("—");
    expect(formatSize(NaN)).toBe("—");
  });
});

describe("formatDate", () => {
  it("treats 0 as unknown rather than 1970", () => {
    expect(formatDate(0)).toBe("—");
  });

  it("shows a time for today and a date otherwise", () => {
    const now = new Date("2026-03-10T15:00:00Z").getTime();
    const earlierToday = new Date("2026-03-10T09:30:00Z").getTime();
    const lastYear = new Date("2025-07-04T09:30:00Z").getTime();

    expect(formatDate(earlierToday, now)).not.toContain("2026");
    expect(formatDate(lastYear, now)).toContain("2025");
  });
});

describe("breadcrumbs", () => {
  it("splits Windows paths", () => {
    expect(breadcrumbs("C:\\Users\\tymon\\Documents")).toEqual([
      { label: "C:", path: "C:" },
      { label: "Users", path: "C:\\Users" },
      { label: "tymon", path: "C:\\Users\\tymon" },
      { label: "Documents", path: "C:\\Users\\tymon\\Documents" },
    ]);
  });

  it("splits POSIX paths and keeps the leading root", () => {
    expect(breadcrumbs("/Users/tymon")).toEqual([
      { label: "Users", path: "/Users" },
      { label: "tymon", path: "/Users/tymon" },
    ]);
  });

  it("handles an empty path", () => {
    expect(breadcrumbs("")).toEqual([]);
  });
});

describe("formatRate and formatEta", () => {
  it("formats throughput per second", () => {
    expect(formatRate(1024 * 1024)).toBe("1.00 MB/s");
    expect(formatRate(0)).toBe("—");
  });

  it("renders an absent estimate as unknown rather than zero", () => {
    expect(formatEta(null)).toBe("—");
    expect(formatEta(45)).toBe("45s");
    expect(formatEta(125)).toBe("2m 5s");
    expect(formatEta(7325)).toBe("2h 2m");
  });
});
