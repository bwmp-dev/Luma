import type { DiffHunk, DiffLine } from "./diff";

/*
 * Line diff between two texts, as hunks in the shape the repository viewer
 * renders. Myers' O((N+M)·D) algorithm after trimming the common prefix and
 * suffix; it gives up (returns null) past `maxEdits` differing lines, because
 * its backtracking memory grows with D², and a diff that large is not readable
 * anyway.
 *
 * Lines keep their terminator while compared, so a changed line ending or a
 * missing final newline counts as a difference, but the rendered text has it
 * stripped.
 */

type Op = {
  kind: DiffLine["kind"];
  token: string;
  oldNumber: number | null;
  newNumber: number | null;
};

function splitLines(text: string): string[] {
  if (text === "") return [];
  return text.split(/(?<=\n)/);
}

function shortestEdit(a: string[], b: string[], maxEdits: number): Op[] | null {
  const n = a.length;
  const m = b.length;
  const max = Math.min(n + m, maxEdits);
  const offset = max + 1;
  const v = new Int32Array(2 * max + 3);
  /* trace[d] holds v for diagonals -d..d after step d. */
  const trace: Int32Array[] = [];

  for (let d = 0; d <= max; d += 1) {
    for (let k = -d; k <= d; k += 2) {
      let x =
        k === -d || (k !== d && v[offset + k - 1] < v[offset + k + 1])
          ? v[offset + k + 1]
          : v[offset + k - 1] + 1;
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x += 1;
        y += 1;
      }
      v[offset + k] = x;
      if (x >= n && y >= m) {
        trace.push(v.slice(offset - d, offset + d + 1));
        return backtrack(a, b, trace);
      }
    }
    trace.push(v.slice(offset - d, offset + d + 1));
  }
  return null;
}

function backtrack(a: string[], b: string[], trace: Int32Array[]): Op[] {
  const ops: Op[] = [];
  let x = a.length;
  let y = b.length;
  const at = (d: number, k: number) => trace[d][k + d];
  const equal = () => {
    ops.push({ kind: "context", token: a[x - 1], oldNumber: x, newNumber: y });
    x -= 1;
    y -= 1;
  };

  for (let d = trace.length - 1; d > 0; d -= 1) {
    const k = x - y;
    const previousK = k === -d || (k !== d && at(d - 1, k - 1) < at(d - 1, k + 1)) ? k + 1 : k - 1;
    const previousX = at(d - 1, previousK);
    const previousY = previousX - previousK;
    while (x > previousX && y > previousY) equal();
    if (x === previousX) {
      ops.push({ kind: "add", token: b[y - 1], oldNumber: null, newNumber: y });
      y -= 1;
    } else {
      ops.push({ kind: "remove", token: a[x - 1], oldNumber: x, newNumber: null });
      x -= 1;
    }
  }
  while (x > 0 && y > 0) equal();
  return ops.reverse();
}

function toLine(op: Op): DiffLine {
  const noNewline = !op.token.endsWith("\n");
  return {
    kind: op.kind,
    text: op.token.replace(/\r?\n$/, ""),
    oldNumber: op.oldNumber,
    newNumber: op.newNumber,
    noNewline,
  };
}

function buildHunks(ops: Op[], context: number): DiffHunk[] {
  const hunks: DiffHunk[] = [];
  let index = 0;
  while (index < ops.length) {
    while (index < ops.length && ops[index].kind === "context") index += 1;
    if (index >= ops.length) break;

    const start = Math.max(0, index - context);
    let end = index;
    // Extend through changes, merging any that are within 2×context of each other.
    for (;;) {
      while (end < ops.length && ops[end].kind !== "context") end += 1;
      let gap = end;
      while (gap < ops.length && ops[gap].kind === "context") gap += 1;
      if (gap < ops.length && gap - end <= context * 2) {
        end = gap;
        continue;
      }
      end = Math.min(ops.length, end + context);
      break;
    }

    const slice = ops.slice(start, end);
    const oldLines = slice.filter((op) => op.kind !== "add").length;
    const newLines = slice.filter((op) => op.kind !== "remove").length;
    const firstOld = slice.find((op) => op.oldNumber !== null)?.oldNumber;
    const firstNew = slice.find((op) => op.newNumber !== null)?.newNumber;
    // An empty side starts at the line before the change, as in unified diffs.
    const oldStart =
      oldLines === 0 ? (previousNumber(ops, start, "oldNumber") ?? 0) : (firstOld ?? 0);
    const newStart =
      newLines === 0 ? (previousNumber(ops, start, "newNumber") ?? 0) : (firstNew ?? 0);
    hunks.push({
      header: `@@ -${oldStart},${oldLines} +${newStart},${newLines} @@`,
      oldStart,
      oldLines,
      newStart,
      newLines,
      lines: slice.map(toLine),
    });
    index = end;
  }
  return hunks;
}

function previousNumber(
  ops: Op[],
  before: number,
  field: "oldNumber" | "newNumber",
): number | null {
  for (let index = before - 1; index >= 0; index -= 1) {
    const value = ops[index][field];
    if (value !== null) return value;
  }
  return null;
}

/**
 * Hunks turning `before` into `after`, or null when they differ in more than
 * `maxEdits` lines. Identical texts produce no hunks.
 */
export function diffLines(
  before: string,
  after: string,
  { context = 3, maxEdits = 2000 }: { context?: number; maxEdits?: number } = {},
): DiffHunk[] | null {
  const a = splitLines(before);
  const b = splitLines(after);
  let prefix = 0;
  while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) prefix += 1;
  let suffix = 0;
  while (
    suffix < a.length - prefix &&
    suffix < b.length - prefix &&
    a[a.length - 1 - suffix] === b[b.length - 1 - suffix]
  ) {
    suffix += 1;
  }

  const middle = shortestEdit(
    a.slice(prefix, a.length - suffix),
    b.slice(prefix, b.length - suffix),
    maxEdits,
  );
  if (middle === null) return null;

  const ops: Op[] = [];
  for (let index = 0; index < prefix; index += 1) {
    ops.push({ kind: "context", token: a[index], oldNumber: index + 1, newNumber: index + 1 });
  }
  for (const op of middle) {
    ops.push({
      ...op,
      oldNumber: op.oldNumber === null ? null : op.oldNumber + prefix,
      newNumber: op.newNumber === null ? null : op.newNumber + prefix,
    });
  }
  for (let index = suffix; index > 0; index -= 1) {
    ops.push({
      kind: "context",
      token: a[a.length - index],
      oldNumber: a.length - index + 1,
      newNumber: b.length - index + 1,
    });
  }
  return buildHunks(ops, context);
}
