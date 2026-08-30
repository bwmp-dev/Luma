import { describe, expect, it } from "vitest";
import { normalizeDialogPath } from "./dialogPath";

describe("normalizeDialogPath", () => {
  it("decodes iOS file picker URLs", () => {
    expect(normalizeDialogPath("file:///private/tmp/My%20File%20%231.json")).toBe(
      "/private/tmp/My File #1.json",
    );
  });

  it("preserves native paths", () => {
    expect(normalizeDialogPath("/private/tmp/My File.json")).toBe("/private/tmp/My File.json");
    expect(normalizeDialogPath(String.raw`C:\Users\Alice\My File.json`)).toBe(
      String.raw`C:\Users\Alice\My File.json`,
    );
  });

  it("preserves malformed and remote file URLs for backend validation", () => {
    expect(normalizeDialogPath("file:///tmp/bad%ZZname")).toBe("file:///tmp/bad%ZZname");
    expect(normalizeDialogPath("file://example.com/tmp/file.txt")).toBe(
      "file://example.com/tmp/file.txt",
    );
  });
});
