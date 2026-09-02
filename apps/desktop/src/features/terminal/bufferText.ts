import type { Terminal } from "@xterm/xterm";

/*
 * Reading text back out of an xterm buffer, following wrapped lines.
 *
 * xterm already joins wrapped rows for its own link detection and double-click
 * word selection, but only from mouse events it handles internally. Touch
 * selection and the pane's link actions need the same reading from the outside,
 * so the buffer walk lives here as plain functions over a Terminal — no DOM, so
 * the range maths is testable against a stub buffer.
 */

/** A position in the buffer: column, and an ABSOLUTE row index (not viewport
 * relative), which is what `Terminal.select` and `IBuffer.getLine` both use. */
export type Cell = { x: number; y: number };

/**
 * One logical line — every buffer row that wraps into the next joined together —
 * alongside the cell each character came from, so a match in `text` maps back to
 * exact buffer coordinates.
 */
export type LogicalLine = {
  text: string;
  /** Same length as `text`; one entry per UTF-16 code unit. */
  cells: Cell[];
};

/**
 * The strict URL pattern @xterm/addon-web-links uses, so a URL these helpers
 * find is exactly the one clicking the terminal would have opened. Global: read
 * it with `matchAll`, which does not mutate its `lastIndex`.
 */
export const URL_PATTERN =
  /(?:https?|HTTPS?):\/\/[^\s"'!*(){}|\\^<>`]*[^\s"':,.!?{}|\\^~[\]`()<>]/g;

const WHOLE_URL = new RegExp(`^(?:${URL_PATTERN.source})$`);

/** Whether `text` is a URL and nothing else, used to decide if a selection can
 * be opened rather than only copied. */
export function isUrlText(text: string): boolean {
  return WHOLE_URL.test(text.trim());
}

/** The logical line containing `row`, or null when the row is out of range. */
export function logicalLineAt(term: Terminal, row: number): LogicalLine | null {
  const buffer = term.buffer.active;
  if (row < 0 || row >= buffer.length) return null;

  let start = row;
  while (start > 0 && buffer.getLine(start)?.isWrapped) start -= 1;
  let end = row;
  while (end + 1 < buffer.length && buffer.getLine(end + 1)?.isWrapped) end += 1;

  const parts: string[] = [];
  const cells: Cell[] = [];
  const cell = buffer.getNullCell();
  for (let y = start; y <= end; y += 1) {
    const line = buffer.getLine(y);
    if (!line) continue;
    for (let x = 0; x < line.length; x += 1) {
      line.getCell(x, cell);
      // Width 0 is the trailing half of a wide character, already emitted with
      // its leading cell; a blank cell reads as "" and stands in as a space so
      // columns keep lining up with the string.
      if (cell.getWidth() === 0) continue;
      const chars = cell.getChars() || " ";
      parts.push(chars);
      for (let i = 0; i < chars.length; i += 1) cells.push({ x, y });
    }
  }
  return { text: parts.join(""), cells };
}

/** Index into `line.text` for a buffer cell, or -1. A click on the trailing half
 * of a wide character resolves to its leading cell. */
export function indexOfCell(line: LogicalLine, cell: Cell): number {
  const exact = line.cells.findIndex((c) => c.y === cell.y && c.x === cell.x);
  if (exact !== -1) return exact;
  return line.cells.findIndex((c) => c.y === cell.y && c.x === cell.x - 1);
}

/** Half-open `[start, end)` range of the URL covering `index`, or null. */
export function urlRangeAt(
  line: LogicalLine,
  index: number,
): [number, number] | null {
  for (const match of line.text.matchAll(URL_PATTERN)) {
    const start = match.index ?? 0;
    const end = start + match[0].length;
    if (index >= start && index < end) return [start, end];
  }
  return null;
}

/**
 * Half-open `[start, end)` range of the whitespace-delimited run covering
 * `index`, or null when the character itself is whitespace.
 *
 * Only whitespace separates: unlike xterm's double-click, brackets and quotes do
 * not split, because on touch this is the fallback for grabbing a path or an
 * unrecognised URL in one go.
 */
export function wordRangeAt(
  line: LogicalLine,
  index: number,
): [number, number] | null {
  const char = line.text[index];
  if (char === undefined || /\s/.test(char)) return null;
  let start = index;
  while (start > 0 && !/\s/.test(line.text[start - 1]!)) start -= 1;
  let end = index + 1;
  while (end < line.text.length && !/\s/.test(line.text[end]!)) end += 1;
  return [start, end];
}

/** Number of cells a `[start, end)` string range spans, and where it begins —
 * the two arguments `Terminal.select` wants. Null when the range is empty. */
export function cellSpan(
  line: LogicalLine,
  range: [number, number],
  cols: number,
): { start: Cell; length: number } | null {
  const first = line.cells[range[0]];
  const last = line.cells[range[1] - 1];
  if (!first || !last) return null;
  return {
    start: first,
    length: (last.y - first.y) * cols + (last.x - first.x) + 1,
  };
}
