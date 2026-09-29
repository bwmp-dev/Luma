import { describe, expect, it } from "vitest";
import { breadcrumbSegments, parentPath } from "./sftp";

describe("breadcrumbSegments", () => {
  it("splits unix paths from the filesystem root", () => {
    expect(breadcrumbSegments("/home/alice", "/")).toEqual([
      { label: "/", path: "/" },
      { label: "home", path: "/home" },
      { label: "alice", path: "/home/alice" },
    ]);
  });

  it("starts Windows paths at the drive root", () => {
    expect(breadcrumbSegments("C:\\Users\\proms", "\\")).toEqual([
      { label: "C:", path: "C:\\" },
      { label: "Users", path: "C:\\Users" },
      { label: "proms", path: "C:\\Users\\proms" },
    ]);
    expect(breadcrumbSegments("C:\\", "\\")).toEqual([{ label: "C:", path: "C:\\" }]);
  });

  it("keeps the verbatim prefix on navigation targets but not on labels", () => {
    expect(breadcrumbSegments("\\\\?\\C:\\Users\\proms", "\\")).toEqual([
      { label: "C:", path: "\\\\?\\C:\\" },
      { label: "Users", path: "\\\\?\\C:\\Users" },
      { label: "proms", path: "\\\\?\\C:\\Users\\proms" },
    ]);
  });

  it("treats a network share as a single root", () => {
    expect(breadcrumbSegments("\\\\nas\\media\\films", "\\")).toEqual([
      { label: "\\\\nas\\media", path: "\\\\nas\\media\\" },
      { label: "films", path: "\\\\nas\\media\\films" },
    ]);
    expect(breadcrumbSegments("\\\\?\\UNC\\nas\\media\\films", "\\")).toEqual([
      { label: "\\\\nas\\media", path: "\\\\?\\UNC\\nas\\media\\" },
      { label: "films", path: "\\\\?\\UNC\\nas\\media\\films" },
    ]);
  });
});

describe("parentPath", () => {
  it("walks unix paths up to the root", () => {
    expect(parentPath("/home/alice", "/")).toBe("/home");
    expect(parentPath("/home", "/")).toBe("/");
    expect(parentPath("/", "/")).toBeNull();
  });

  it("walks Windows drive paths up to the drive root", () => {
    expect(parentPath("C:\\Users\\proms", "\\")).toBe("C:\\Users");
    expect(parentPath("C:\\Users", "\\")).toBe("C:\\");
    expect(parentPath("C:\\", "\\")).toBeNull();
  });

  it("stops at verbatim and share roots", () => {
    expect(parentPath("\\\\?\\C:\\Users", "\\")).toBe("\\\\?\\C:\\");
    expect(parentPath("\\\\?\\C:\\", "\\")).toBeNull();
    expect(parentPath("\\\\nas\\media\\films", "\\")).toBe("\\\\nas\\media\\");
    expect(parentPath("\\\\nas\\media", "\\")).toBeNull();
    expect(parentPath("\\\\?\\UNC\\nas\\media\\films", "\\")).toBe("\\\\?\\UNC\\nas\\media\\");
    expect(parentPath("\\\\?\\UNC\\nas\\media\\", "\\")).toBeNull();
  });
});
