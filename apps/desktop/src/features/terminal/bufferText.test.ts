import { describe, it, expect } from "vitest";
import type { Terminal } from "@xterm/xterm";
import {
  cellSpan,
  indexOfCell,
  isUrlText,
  logicalLineAt,
  urlRangeAt,
  wordRangeAt,
} from "./bufferText";

/*
 * A stub buffer is enough here: every helper reads through IBuffer/IBufferLine,
 * so the interesting cases — a URL split across wrapped rows, a wide character
 * occupying two cells — are expressed as plain rows of text.
 */

const COLS = 12;
/** Stands in for any double-width character; the stub gives it two cells. */
const WIDE = "Ａ";

type StubCell = { chars: string; width: number };

/** One row expanded to COLS cells, with wide characters taking two (the second
 * reporting width 0, exactly as xterm does). */
function cellsOf(row: string): StubCell[] {
  const cells: StubCell[] = [];
  for (const char of row) {
    cells.push({ chars: char, width: char === WIDE ? 2 : 1 });
    if (char === WIDE) cells.push({ chars: "", width: 0 });
  }
  while (cells.length < COLS) cells.push({ chars: "", width: 1 });
  return cells.slice(0, COLS);
}

/** A terminal stub whose buffer is `rows`, with the indices in `wrapped` marked
 * as continuations of the row above. */
function stub(rows: string[], wrapped: number[] = []): Terminal {
  const lines = rows.map((row, index) => {
    const cells = cellsOf(row);
    return {
      isWrapped: wrapped.includes(index),
      length: COLS,
      getCell(x: number, target: StubCell) {
        target.chars = cells[x]?.chars ?? "";
        target.width = cells[x]?.width ?? 1;
      },
    };
  });
  return {
    cols: COLS,
    buffer: {
      active: {
        length: lines.length,
        getLine: (y: number) => lines[y],
        getNullCell: () => {
          const cell = {
            chars: "",
            width: 1,
            getChars: () => cell.chars,
            getWidth: () => cell.width,
          };
          return cell;
        },
      },
    },
  } as unknown as Terminal;
}

/** "see https://" / "example.com/" / "a b" — one URL broken over three rows. */
const wrappedUrl = () =>
  stub(["see https://", "example.com/", "a b"], [1, 2]);

describe("logicalLineAt", () => {
  it("joins wrapped rows and maps every character back to its cell", () => {
    const line = logicalLineAt(stub(["abcdefghijkl", "mno"], [1]), 1);
    expect(line?.text.startsWith("abcdefghijklmno")).toBe(true);
    expect(line?.cells[0]).toEqual({ x: 0, y: 0 });
    expect(line?.cells[12]).toEqual({ x: 0, y: 1 });
    expect(line?.cells.length).toBe(line?.text.length);
  });

  it("stops at rows that are not continuations", () => {
    const line = logicalLineAt(stub(["one", "two", "three"], [2]), 1);
    expect(line?.text.startsWith("two")).toBe(true);
    expect(line?.text).toContain("three");
    expect(line?.text).not.toContain("one");
  });

  it("returns null outside the buffer", () => {
    expect(logicalLineAt(stub(["one"]), 4)).toBeNull();
  });
});

describe("urlRangeAt", () => {
  it("finds a URL that continues onto a wrapped row", () => {
    const line = logicalLineAt(wrappedUrl(), 0)!;
    const range = urlRangeAt(line, indexOfCell(line, { x: 2, y: 1 }));
    expect(range && line.text.slice(...range)).toBe("https://example.com/a");
  });

  it("resolves the same URL whichever row was hit", () => {
    const line = logicalLineAt(wrappedUrl(), 2)!;
    expect(urlRangeAt(line, indexOfCell(line, { x: 6, y: 0 }))).toEqual(
      urlRangeAt(line, indexOfCell(line, { x: 0, y: 2 })),
    );
  });

  it("ignores positions outside any URL", () => {
    const line = logicalLineAt(wrappedUrl(), 0)!;
    expect(urlRangeAt(line, indexOfCell(line, { x: 0, y: 0 }))).toBeNull();
  });
});

describe("wordRangeAt", () => {
  it("takes the whole whitespace-delimited run, brackets included", () => {
    const line = logicalLineAt(stub(["cd /var/(x)", "y"], [1]), 0)!;
    const range = wordRangeAt(line, indexOfCell(line, { x: 5, y: 0 }));
    expect(range && line.text.slice(...range)).toBe("/var/(x)");
  });

  it("returns null on whitespace", () => {
    const line = logicalLineAt(stub(["a b"]), 0)!;
    expect(wordRangeAt(line, 1)).toBeNull();
  });
});

describe("cellSpan", () => {
  it("counts cells across wraps so Terminal.select covers every row", () => {
    const line = logicalLineAt(wrappedUrl(), 0)!;
    const range = urlRangeAt(line, indexOfCell(line, { x: 4, y: 0 }))!;
    const span = cellSpan(line, range, COLS)!;
    expect(span.start).toEqual({ x: 4, y: 0 });
    expect(span.length).toBe("https://example.com/a".length);
  });
});

describe("indexOfCell", () => {
  it("resolves the trailing half of a wide character to its leading cell", () => {
    const line = logicalLineAt(stub([`${WIDE}b`]), 0)!;
    expect(indexOfCell(line, { x: 1, y: 0 })).toBe(0);
  });
});

describe("isUrlText", () => {
  it("accepts a bare URL and rejects anything around it", () => {
    expect(isUrlText("https://example.com/a")).toBe(true);
    expect(isUrlText("  https://example.com  ")).toBe(true);
    expect(isUrlText("see https://example.com")).toBe(false);
    expect(isUrlText("example.com")).toBe(false);
  });
});
