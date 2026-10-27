import { describe, expect, it } from "vitest";
import type { DiffHunk } from "./diff";
import { diffLines } from "./lineDiff";

/** Rebuild the "after" text from "before" and the hunks, the way `patch` would. */
function applyHunks(before: string, hunks: DiffHunk[]): string {
  const lines = before === "" ? [] : before.split(/(?<=\n)/);
  const out: string[] = [];
  let cursor = 0;
  for (const hunk of hunks) {
    const start = hunk.oldLines === 0 ? hunk.oldStart : hunk.oldStart - 1;
    out.push(...lines.slice(cursor, start));
    cursor = start;
    for (const line of hunk.lines) {
      const token = line.noNewline ? line.text : null;
      if (line.kind === "remove" || line.kind === "context") {
        cursor += 1;
      }
      if (line.kind === "add" || line.kind === "context") {
        // The original token (with its own line ending) is what gets kept.
        const original = line.kind === "context" ? lines[cursor - 1] : null;
        out.push(original ?? token ?? `${line.text}\n`);
      }
    }
  }
  out.push(...lines.slice(cursor));
  return out.join("");
}

function seeded(seed: number) {
  let state = seed;
  return () => {
    state = (state * 1103515245 + 12345) % 2147483648;
    return state / 2147483648;
  };
}

describe("diffLines", () => {
  it("returns no hunks for identical text", () => {
    expect(diffLines("a\nb\n", "a\nb\n")).toEqual([]);
    expect(diffLines("", "")).toEqual([]);
  });

  it("numbers both sides and keeps three lines of context", () => {
    const before = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
    const after = "1\n2\n3\n4\nfive\n6\n7\n8\n9\n";
    const hunks = diffLines(before, after)!;
    expect(hunks).toHaveLength(1);
    expect(hunks[0].header).toBe("@@ -2,7 +2,7 @@");
    expect(hunks[0].lines.map((line) => [line.kind, line.text])).toEqual([
      ["context", "2"],
      ["context", "3"],
      ["context", "4"],
      ["remove", "5"],
      ["add", "five"],
      ["context", "6"],
      ["context", "7"],
      ["context", "8"],
    ]);
    expect(hunks[0].lines[3]).toMatchObject({ oldNumber: 5, newNumber: null });
    expect(hunks[0].lines[4]).toMatchObject({ oldNumber: null, newNumber: 5 });
  });

  it("splits distant changes into separate hunks and merges close ones", () => {
    const base = Array.from({ length: 30 }, (_, index) => `line ${index + 1}\n`);
    const far = [...base];
    far[1] = "changed\n";
    far[25] = "changed\n";
    expect(diffLines(base.join(""), far.join(""))).toHaveLength(2);

    const near = [...base];
    near[1] = "changed\n";
    near[6] = "changed\n";
    expect(diffLines(base.join(""), near.join(""))).toHaveLength(1);
  });

  it("treats a changed line ending or missing final newline as a change", () => {
    const endings = diffLines("a\r\nb\r\n", "a\nb\r\n")!;
    expect(endings[0].lines.map((line) => line.kind)).toEqual(["remove", "add", "context"]);
    expect(endings[0].lines[0].text).toBe("a");

    const finalNewline = diffLines("a\nb\n", "a\nb")!;
    const added = finalNewline[0].lines.find((line) => line.kind === "add")!;
    expect(added).toMatchObject({ text: "b", noNewline: true });
  });

  it("handles insertions into and deletions from empty text", () => {
    expect(diffLines("", "x\n")![0].header).toBe("@@ -0,0 +1,1 @@");
    expect(diffLines("x\n", "")![0].header).toBe("@@ -1,1 +0,0 @@");
  });

  it("gives up instead of exhausting memory on a huge rewrite", () => {
    const before = Array.from({ length: 500 }, (_, index) => `a${index}\n`).join("");
    const after = Array.from({ length: 500 }, (_, index) => `b${index}\n`).join("");
    expect(diffLines(before, after, { maxEdits: 100 })).toBeNull();
    expect(diffLines(before, after)).not.toBeNull();
  });

  it("always produces hunks that rebuild the new text", () => {
    const random = seeded(42);
    const pick = () =>
      ["alpha\n", "beta\n", "gamma\n", "delta\n", "\n", "end"][Math.floor(random() * 6)];
    for (let round = 0; round < 300; round += 1) {
      const before = Array.from({ length: Math.floor(random() * 20) }, pick).join("");
      const after = Array.from({ length: Math.floor(random() * 20) }, pick).join("");
      const hunks = diffLines(before, after, { context: Math.floor(random() * 4) })!;
      expect(applyHunks(before, hunks)).toBe(after);
    }
  });
});
