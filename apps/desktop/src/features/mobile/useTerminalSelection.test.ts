import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { bindTerminalSelection } from "./useTerminalSelection";
import { terminalManager } from "../terminal/terminalManager";

/*
 * The DOM half of touch selection: that a drag turns into a cell range, that a
 * tap falls through to selectAt (URL/word), and that neither reaches xterm or
 * scrolls the viewport. The cell maths itself is terminalManager's, covered in
 * bufferText.test.ts.
 *
 * jsdom has no TouchEvent constructor, so events carry only the fields the
 * listeners read.
 */

type Point = { x: number; y: number };

function dispatch(target: Element, type: string, points: Point[]): Event {
  const event = new Event(type, { bubbles: true, cancelable: true });
  const toTouch = (p: Point) => ({ clientX: p.x, clientY: p.y }) as Touch;
  Object.defineProperty(event, "touches", { value: points.map(toTouch) });
  Object.defineProperty(event, "changedTouches", { value: points.map(toTouch) });
  target.dispatchEvent(event);
  return event;
}

let host: HTMLDivElement;
/** Stands in for xterm's own listeners, which live below the host. */
let child: HTMLDivElement;
let childSaw: string[];
let unbind: () => void;
let selectCells: ReturnType<typeof vi.spyOn>;
let selectAt: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  vi.useFakeTimers();
  childSaw = [];
  host = document.createElement("div");
  child = document.createElement("div");
  host.appendChild(child);
  document.body.appendChild(host);
  for (const type of ["touchstart", "touchmove", "touchend"]) {
    child.addEventListener(type, () => childSaw.push(type));
  }
  // One cell per 10px, so a point maps to a predictable buffer position.
  vi.spyOn(terminalManager, "cellAtPoint").mockImplementation((_id, x, y) => ({
    x: Math.floor(x / 10),
    y: Math.floor(y / 10),
  }));
  selectCells = vi.spyOn(terminalManager, "selectCells").mockImplementation(() => {});
  selectAt = vi.spyOn(terminalManager, "selectAt").mockImplementation(() => {});
  vi.spyOn(terminalManager, "scrollLines").mockImplementation(() => {});
  unbind = bindTerminalSelection(host, "s1");
});

afterEach(() => {
  unbind?.();
  host.remove();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("bindTerminalSelection", () => {
  it("selects the range a drag covers, from the press cell", () => {
    dispatch(child, "touchstart", [{ x: 15, y: 25 }]);
    dispatch(child, "touchmove", [{ x: 95, y: 65 }]);

    expect(selectCells).toHaveBeenCalledWith(
      "s1",
      { x: 1, y: 2 },
      { x: 9, y: 6 },
    );
  });

  it("treats a press that never moved as a tap on whatever is under it", () => {
    dispatch(child, "touchstart", [{ x: 15, y: 25 }]);
    dispatch(child, "touchend", [{ x: 17, y: 26 }]);

    expect(selectAt).toHaveBeenCalledWith("s1", { x: 1, y: 2 });
    expect(selectCells).not.toHaveBeenCalled();
  });

  it("does not re-select on release after a drag", () => {
    dispatch(child, "touchstart", [{ x: 15, y: 25 }]);
    dispatch(child, "touchmove", [{ x: 95, y: 65 }]);
    dispatch(child, "touchend", [{ x: 95, y: 65 }]);

    expect(selectAt).not.toHaveBeenCalled();
  });

  it("keeps the touch away from xterm and from the viewport scroll", () => {
    const start = dispatch(child, "touchstart", [{ x: 15, y: 25 }]);
    childSaw = [];
    const move = dispatch(child, "touchmove", [{ x: 95, y: 65 }]);
    const end = dispatch(child, "touchend", [{ x: 95, y: 65 }]);

    expect(start.defaultPrevented).toBe(true);
    expect(move.defaultPrevented).toBe(true);
    expect(end.defaultPrevented).toBe(true);
    expect(childSaw).toEqual([]);
  });

  it("stands aside for a second finger", () => {
    const event = dispatch(child, "touchstart", [
      { x: 15, y: 25 },
      { x: 40, y: 25 },
    ]);

    expect(event.defaultPrevented).toBe(false);
    dispatch(child, "touchmove", [{ x: 95, y: 65 }]);
    expect(selectCells).not.toHaveBeenCalled();
  });

  it("scrolls and keeps extending while the finger rests at the bottom edge", () => {
    host.getBoundingClientRect = () =>
      ({ top: 0, bottom: 100, left: 0, right: 200 }) as DOMRect;

    dispatch(child, "touchstart", [{ x: 15, y: 25 }]);
    dispatch(child, "touchmove", [{ x: 95, y: 95 }]);
    selectCells.mockClear();
    vi.advanceTimersByTime(200);

    expect(terminalManager.scrollLines).toHaveBeenCalledWith("s1", 1);
    expect(selectCells).toHaveBeenCalled();
  });

  it("stops edge scrolling once the listeners are removed", () => {
    host.getBoundingClientRect = () =>
      ({ top: 0, bottom: 100, left: 0, right: 200 }) as DOMRect;
    dispatch(child, "touchstart", [{ x: 15, y: 25 }]);
    dispatch(child, "touchmove", [{ x: 95, y: 95 }]);

    unbind();
    vi.mocked(terminalManager.scrollLines).mockClear();
    vi.advanceTimersByTime(200);

    expect(terminalManager.scrollLines).not.toHaveBeenCalled();
  });
});
