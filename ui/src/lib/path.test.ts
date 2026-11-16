import { describe, expect, it } from "vitest";
import {
  basename,
  invalidNameReason,
  isRoot,
  isWithin,
  join,
  normalize,
  parent,
  stemLength,
  uniqueName,
  volumeOf,
} from "./path";

describe("normalize", () => {
  it("keeps a drive root's backslash and trims others", () => {
    expect(normalize("c:")).toBe("C:\\");
    expect(normalize("C:\\")).toBe("C:\\");
    expect(normalize("C:\\Users\\")).toBe("C:\\Users");
    expect(normalize("C:/Users//alex/")).toBe("C:\\Users\\alex".replace(/\\/g, "\\"));
  });

  it("preserves the UNC prefix", () => {
    expect(normalize("\\\\server\\share\\dir\\")).toBe("\\\\server\\share\\dir");
  });

  it("handles POSIX", () => {
    expect(normalize("/Users//alex/")).toBe("/Users/alex");
    expect(normalize("/")).toBe("/");
  });
});

describe("parent and basename", () => {
  it("walks up Windows paths and stops at the root", () => {
    expect(parent("C:\\Users\\alex")).toBe("C:\\Users");
    expect(parent("C:\\Users")).toBe("C:\\");
    expect(parent("C:\\")).toBeNull();
    expect(basename("C:\\Users\\alex")).toBe("alex");
    expect(basename("C:\\")).toBe("C:");
  });

  it("walks up POSIX and UNC paths", () => {
    expect(parent("/Users")).toBe("/");
    expect(parent("/")).toBeNull();
    expect(parent("\\\\srv\\share\\x")).toBe("\\\\srv\\share");
    expect(isRoot("\\\\srv\\share")).toBe(true);
  });
});

describe("join", () => {
  it("never doubles a separator", () => {
    expect(join("C:\\", "Users")).toBe("C:\\Users");
    expect(join("C:\\Users", "alex")).toBe("C:\\Users\\alex");
    expect(join("/", "tmp")).toBe("/tmp");
  });
});

describe("isWithin and volumeOf", () => {
  it("is case-insensitive and boundary-aware", () => {
    expect(isWithin("C:\\Users\\Alex\\x", "c:\\users")).toBe(true);
    expect(isWithin("C:\\Users", "C:\\Users")).toBe(true);
    expect(isWithin("C:\\UsersX", "C:\\Users")).toBe(false);
    expect(isWithin("C:\\anything", "C:\\")).toBe(true);
  });

  it("extracts the drive letter", () => {
    expect(volumeOf("d:\\x")).toBe("D");
    expect(volumeOf("\\\\srv\\share")).toBeNull();
  });
});

describe("naming", () => {
  it("picks Explorer-style unique names", () => {
    expect(uniqueName("New folder", new Set(["a"]))).toBe("New folder");
    expect(uniqueName("New folder", new Set(["new folder", "New folder (2)"]))).toBe("New folder (3)");
    expect(uniqueName("a.txt", new Set(["A.TXT"]))).toBe("a (2).txt");
  });

  it("rejects names Windows would refuse", () => {
    expect(invalidNameReason("ok.txt")).toBeNull();
    expect(invalidNameReason("  ")).not.toBeNull();
    expect(invalidNameReason("a:b")).not.toBeNull();
    expect(invalidNameReason("trailing.")).not.toBeNull();
    expect(invalidNameReason("CON")).not.toBeNull();
    expect(invalidNameReason("con.txt")).not.toBeNull();
    expect(invalidNameReason("console.txt")).toBeNull();
  });

  it("pre-selects the stem, not the extension", () => {
    expect(stemLength("report.final.pdf", false)).toBe(12);
    expect(stemLength(".gitignore", false)).toBe(10);
    expect(stemLength("folder.v2", true)).toBe(9);
  });
});
