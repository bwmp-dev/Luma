import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { terminalManager } from "../terminal/terminalManager";
import { bindTerminalScroll } from "./useTerminalScroll";

type Point = { x: number; y: number };

function dispatch(target: Element, type: string, points: Point[]): Event {
  const event = new Event(type, { bubbles: true, cancelable: true });
  const toTouch = (point: Point) =>
    ({ clientX: point.x, clientY: point.y }) as Touch;
  Object.defineProperty(event, "touches", { value: points.map(toTouch) });
  Object.defineProperty(event, "changedTouches", { value: points.map(toTouch) });
  target.dispatchEvent(event);
  return event;
}

let host: HTMLDivElement;
let child: HTMLDivElement;
let childSawMove: boolean;
let unbind: (() => void) | null;

function bind(oneFinger = true) {
  unbind?.();
  unbind = bindTerminalScroll(host, "s1", { oneFinger });
}

beforeEach(() => {
  host = document.createElement("div");
  child = document.createElement("div");
  host.appendChild(child);
  document.body.appendChild(host);
  childSawMove = false;
  child.addEventListener("touchmove", () => {
    childSawMove = true;
  });
  vi.spyOn(terminalManager, "cellHeight").mockReturnValue(10);
  vi.spyOn(terminalManager, "scrollLines").mockImplementation(() => {});
  unbind = null;
  bind();
});

afterEach(() => {
  unbind?.();
  host.remove();
  vi.restoreAllMocks();
});

describe("bindTerminalScroll", () => {
  it("scrolls back when one finger drags down", () => {
    dispatch(child, "touchstart", [{ x: 20, y: 20 }]);
    const move = dispatch(child, "touchmove", [{ x: 20, y: 50 }]);

    expect(terminalManager.scrollLines).toHaveBeenCalledWith("s1", -3);
    expect(move.defaultPrevented).toBe(true);
    expect(childSawMove).toBe(false);
  });

  it("scrolls forward when one finger drags up", () => {
    dispatch(child, "touchstart", [{ x: 20, y: 80 }]);
    dispatch(child, "touchmove", [{ x: 20, y: 50 }]);

    expect(terminalManager.scrollLines).toHaveBeenCalledWith("s1", 3);
  });

  it("leaves a drag inside the slop to the other recognizers", () => {
    dispatch(child, "touchstart", [{ x: 20, y: 20 }]);
    const move = dispatch(child, "touchmove", [{ x: 20, y: 26 }]);

    expect(terminalManager.scrollLines).not.toHaveBeenCalled();
    expect(move.defaultPrevented).toBe(false);
    expect(childSawMove).toBe(true);
  });

  it("ignores one finger when another recognizer owns it", () => {
    bind(false);
    dispatch(child, "touchstart", [{ x: 20, y: 20 }]);
    const move = dispatch(child, "touchmove", [{ x: 20, y: 60 }]);

    expect(terminalManager.scrollLines).not.toHaveBeenCalled();
    expect(move.defaultPrevented).toBe(false);
  });

  it("scrolls on two fingers even when one finger is spoken for", () => {
    bind(false);
    dispatch(child, "touchstart", [
      { x: 20, y: 20 },
      { x: 50, y: 30 },
    ]);
    const move = dispatch(child, "touchmove", [
      { x: 20, y: 43 },
      { x: 50, y: 53 },
    ]);

    expect(terminalManager.scrollLines).toHaveBeenCalledWith("s1", -2);
    expect(move.defaultPrevented).toBe(true);
  });

  it("accumulates movement smaller than one row", () => {
    dispatch(child, "touchstart", [{ x: 20, y: 20 }]);
    // Clears the slop without reaching a whole row.
    dispatch(child, "touchmove", [{ x: 20, y: 29 }]);
    expect(terminalManager.scrollLines).not.toHaveBeenCalled();

    dispatch(child, "touchmove", [{ x: 20, y: 31 }]);
    expect(terminalManager.scrollLines).toHaveBeenCalledWith("s1", -1);
  });

  it("does not scroll when the two fingers only pinch", () => {
    dispatch(child, "touchstart", [
      { x: 20, y: 20 },
      { x: 50, y: 40 },
    ]);
    dispatch(child, "touchmove", [
      { x: 20, y: 10 },
      { x: 50, y: 50 },
    ]);

    expect(terminalManager.scrollLines).not.toHaveBeenCalled();
  });

  it("re-anchors instead of jumping when a second finger lands", () => {
    dispatch(child, "touchstart", [{ x: 20, y: 20 }]);
    dispatch(child, "touchstart", [
      { x: 20, y: 20 },
      { x: 50, y: 120 },
    ]);
    // The mean moved 50px when the finger landed; only the 10px since counts.
    dispatch(child, "touchmove", [
      { x: 20, y: 30 },
      { x: 50, y: 130 },
    ]);

    expect(terminalManager.scrollLines).toHaveBeenCalledTimes(1);
    expect(terminalManager.scrollLines).toHaveBeenCalledWith("s1", -1);
  });

  it("carries on with the finger left behind when the other lifts", () => {
    dispatch(child, "touchstart", [
      { x: 20, y: 20 },
      { x: 50, y: 120 },
    ]);
    dispatch(child, "touchmove", [
      { x: 20, y: 40 },
      { x: 50, y: 140 },
    ]);
    vi.mocked(terminalManager.scrollLines).mockClear();

    dispatch(child, "touchend", [{ x: 20, y: 40 }]);
    dispatch(child, "touchmove", [{ x: 20, y: 60 }]);

    expect(terminalManager.scrollLines).toHaveBeenCalledWith("s1", -2);
  });

  it("starts the next gesture from scratch after an interrupted one", () => {
    bind(false);
    // Two fingers scroll, then one lifts: what is left cannot scroll on its own.
    dispatch(child, "touchstart", [
      { x: 20, y: 20 },
      { x: 50, y: 120 },
    ]);
    dispatch(child, "touchmove", [
      { x: 20, y: 60 },
      { x: 50, y: 160 },
    ]);
    dispatch(child, "touchend", [{ x: 20, y: 60 }]);
    dispatch(child, "touchend", []);
    vi.mocked(terminalManager.scrollLines).mockClear();

    // The next two-finger drag has to clear the slop again rather than picking
    // up where the last one left off.
    dispatch(child, "touchstart", [
      { x: 20, y: 20 },
      { x: 50, y: 120 },
    ]);
    const move = dispatch(child, "touchmove", [
      { x: 20, y: 23 },
      { x: 50, y: 123 },
    ]);

    expect(terminalManager.scrollLines).not.toHaveBeenCalled();
    expect(move.defaultPrevented).toBe(false);
  });

  it("stops responding after teardown", () => {
    dispatch(child, "touchstart", [{ x: 20, y: 20 }]);
    unbind?.();
    unbind = null;
    dispatch(child, "touchmove", [{ x: 20, y: 60 }]);

    expect(terminalManager.scrollLines).not.toHaveBeenCalled();
  });
});
