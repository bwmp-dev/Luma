import { describe, it, expect, afterEach } from "vitest";
import { resolvedAccentColor } from "./tabBar";

function setAccent(value: string): void {
  document.documentElement.style.setProperty("--accent", value);
}

afterEach(() => {
  document.documentElement.style.removeProperty("--accent");
});

describe("resolvedAccentColor", () => {
  it("drops the alpha byte from an #rrggbbaa accent", () => {
    // A theme importing #ff000080 used to reach UIColor as 8 digits, which the
    // plugin rejected outright -- leaving the bar on the previous theme's tint.
    setAccent("#ff000080");
    expect(resolvedAccentColor()).toBe("#ff0000");
  });

  it("passes #rrggbb through unchanged", () => {
    setAccent("#3b82f6");
    expect(resolvedAccentColor()).toBe("#3b82f6");
  });

  it("expands shorthand #rgb", () => {
    setAccent("#0f8");
    expect(resolvedAccentColor()).toBe("#00ff88");
  });

  it("returns null for values UIColor cannot parse", () => {
    for (const value of ["rgb(255, 0, 0)", "red", "#ff00", ""]) {
      setAccent(value);
      expect(resolvedAccentColor()).toBeNull();
    }
  });
});
