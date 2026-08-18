import { describe, it, expect } from "vitest";
import { rowsInBand, scrollRowIntoView } from "./useVirtualRows";

const ROW = 36;

describe("rowsInBand", () => {
  it("covers every row the band overlaps, inclusive at both edges", () => {
    // A band from the middle of row 1 to the middle of row 3 touches 1, 2, 3.
    expect(rowsInBand(ROW * 1.5, ROW * 3.5, ROW, 10)).toEqual({
      from: 1,
      to: 3,
    });
  });

  it("clamps to the list, so a band dragged past the end selects no phantoms", () => {
    expect(rowsInBand(-500, ROW * 100, ROW, 4)).toEqual({ from: 0, to: 3 });
  });

  it("returns an empty range for an empty list", () => {
    const { from, to } = rowsInBand(0, ROW * 5, ROW, 0);
    expect(to).toBeLessThan(from);
  });

  it("selects a single row for a zero-height band inside it", () => {
    expect(rowsInBand(ROW * 2 + 5, ROW * 2 + 5, ROW, 10)).toEqual({
      from: 2,
      to: 2,
    });
  });
});

describe("scrollRowIntoView", () => {
  function node(scrollTop: number, clientHeight = ROW * 10) {
    return { scrollTop, clientHeight } as HTMLElement;
  }

  it("leaves a row already in view alone", () => {
    const el = node(ROW * 5);
    scrollRowIntoView(el, 8, ROW);
    expect(el.scrollTop).toBe(ROW * 5);
  });

  it("scrolls up to a row above the viewport", () => {
    const el = node(ROW * 5);
    scrollRowIntoView(el, 2, ROW);
    expect(el.scrollTop).toBe(ROW * 2);
  });

  it("scrolls down just far enough to reveal a row below the viewport", () => {
    const el = node(ROW * 5);
    scrollRowIntoView(el, 20, ROW);
    // The row's bottom sits flush with the viewport's bottom.
    expect(el.scrollTop).toBe(ROW * 21 - ROW * 10);
  });
});
