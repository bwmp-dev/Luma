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
let unbind: () => void;

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
  unbind = bindTerminalScroll(host, "s1");
});

afterEach(() => {
  unbind();
  host.remove();
  vi.restoreAllMocks();
});

describe("bindTerminalScroll", () => {
  it("scrolls back when two fingers drag down", () => {
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
    expect(childSawMove).toBe(false);
  });

  it("accumulates movement smaller than one row", () => {
    dispatch(child, "touchstart", [
      { x: 20, y: 20 },
      { x: 50, y: 30 },
    ]);
    dispatch(child, "touchmove", [
      { x: 20, y: 26 },
      { x: 50, y: 36 },
    ]);
    expect(terminalManager.scrollLines).not.toHaveBeenCalled();

    dispatch(child, "touchmove", [
      { x: 20, y: 32 },
      { x: 50, y: 42 },
    ]);
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

  it("leaves one-finger movement untouched", () => {
    dispatch(child, "touchstart", [{ x: 20, y: 20 }]);
    const move = dispatch(child, "touchmove", [{ x: 20, y: 50 }]);

    expect(terminalManager.scrollLines).not.toHaveBeenCalled();
    expect(move.defaultPrevented).toBe(false);
    expect(childSawMove).toBe(true);
  });

  it("stops responding after teardown", () => {
    dispatch(child, "touchstart", [
      { x: 20, y: 20 },
      { x: 50, y: 30 },
    ]);
    unbind();
    dispatch(child, "touchmove", [
      { x: 20, y: 50 },
      { x: 50, y: 60 },
    ]);

    expect(terminalManager.scrollLines).not.toHaveBeenCalled();
  });
});
