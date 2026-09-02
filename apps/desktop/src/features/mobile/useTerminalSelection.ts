import { useEffect } from "react";
import { terminalManager } from "../terminal/terminalManager";

/*
 * Touch text selection for the mobile terminal.
 *
 * xterm's selection is driven by mouse events, which a touch only synthesizes
 * after the finger lifts — so dragging across the output selects nothing, and
 * the long press that would raise iOS's own copy callout is already spoken for
 * by the arrow pad (see terminalGestures.ts). This binds the missing gesture
 * directly: while selection mode is on, one finger drags out a selection and a
 * tap grabs the whole URL (or whitespace-delimited run) under it, wrapped rows
 * included.
 *
 * Selection mode is exclusive with the arrow-pad/double-tap gestures, so the two
 * recognizers are never bound to the same host at once.
 */

/** Movement before a press becomes a drag rather than a tap. */
const DRAG_SLOP_PX = 6;
/** Distance from the pane edge at which a drag starts scrolling the viewport. */
const EDGE_PX = 44;
/** One line per tick while the finger rests in the edge zone. */
const EDGE_SCROLL_MS = 60;

/**
 * Attach the selection listeners to `host`, the element the terminal is
 * attached into.
 * @returns a teardown that removes them and stops any edge scrolling.
 */
export function bindTerminalSelection(
  host: HTMLElement,
  sessionId: string,
): () => void {
  /** Where the current press started, in buffer coordinates. Absolute rows, so
   * it survives the viewport scrolling underneath the finger. */
  let anchor: { x: number; y: number } | null = null;
  let origin: { x: number; y: number } | null = null;
  let latest: { x: number; y: number } | null = null;
  let dragging = false;
  let edgeTimer: number | null = null;
  let edgeDirection = 0;

  const stopEdgeScroll = () => {
    if (edgeTimer !== null) window.clearInterval(edgeTimer);
    edgeTimer = null;
    edgeDirection = 0;
  };

  const extendTo = (clientX: number, clientY: number) => {
    if (!anchor) return;
    const cell = terminalManager.cellAtPoint(sessionId, clientX, clientY);
    if (cell) terminalManager.selectCells(sessionId, anchor, cell);
  };

  const updateEdgeScroll = (clientY: number) => {
    const rect = host.getBoundingClientRect();
    const direction =
      clientY < rect.top + EDGE_PX ? -1 : clientY > rect.bottom - EDGE_PX ? 1 : 0;
    if (direction === edgeDirection) return;
    stopEdgeScroll();
    if (direction === 0) return;
    edgeDirection = direction;
    edgeTimer = window.setInterval(() => {
      terminalManager.scrollLines(sessionId, direction);
      if (latest) extendTo(latest.x, latest.y);
    }, EDGE_SCROLL_MS);
  };

  const reset = () => {
    stopEdgeScroll();
    anchor = null;
    origin = null;
    latest = null;
    dragging = false;
  };

  const onTouchStart = (event: TouchEvent) => {
    if (event.touches.length > 1) {
      // A second finger is a pinch: abandon the press and leave the gesture to
      // the webview rather than selecting something the user did not aim at.
      reset();
      return;
    }
    const touch = event.touches[0];
    if (!touch) return;
    origin = { x: touch.clientX, y: touch.clientY };
    latest = origin;
    anchor = terminalManager.cellAtPoint(sessionId, touch.clientX, touch.clientY);
    dragging = false;
    if (!anchor) return;
    // Claim the gesture up front: this is what stops the viewport scrolling
    // under the drag and suppresses the click that would raise the keyboard.
    event.preventDefault();
    event.stopPropagation();
  };

  const onTouchMove = (event: TouchEvent) => {
    const touch = event.touches[0];
    if (!anchor || !origin || !touch) return;
    latest = { x: touch.clientX, y: touch.clientY };
    event.preventDefault();
    event.stopPropagation();
    if (
      !dragging &&
      Math.hypot(touch.clientX - origin.x, touch.clientY - origin.y) <=
        DRAG_SLOP_PX
    ) {
      return;
    }
    dragging = true;
    extendTo(touch.clientX, touch.clientY);
    updateEdgeScroll(touch.clientY);
  };

  const onTouchEnd = (event: TouchEvent) => {
    const cell = anchor;
    const wasDragging = dragging;
    reset();
    if (!cell) return;
    event.preventDefault();
    event.stopPropagation();
    // A press that never moved is a tap: take the whole thing under the finger
    // (URL first) rather than the single cell a zero-length drag would give.
    if (!wasDragging) terminalManager.selectAt(sessionId, cell);
  };

  const active = { capture: true, passive: false } as const;
  host.addEventListener("touchstart", onTouchStart, active);
  host.addEventListener("touchmove", onTouchMove, active);
  host.addEventListener("touchend", onTouchEnd, active);
  host.addEventListener("touchcancel", reset, { capture: true, passive: true });

  return () => {
    host.removeEventListener("touchstart", onTouchStart, { capture: true });
    host.removeEventListener("touchmove", onTouchMove, { capture: true });
    host.removeEventListener("touchend", onTouchEnd, { capture: true });
    host.removeEventListener("touchcancel", reset, { capture: true });
    stopEdgeScroll();
  };
}

/**
 * React binding: keeps the selection listeners attached while selection mode is
 * on, and leaves the terminal unselected and unfocused around it.
 */
export function useTerminalSelection({
  sessionId,
  hostRef,
  enabled,
}: {
  sessionId: string;
  /** The element terminalManager attached the terminal into. */
  hostRef: React.RefObject<HTMLDivElement | null>;
  enabled: boolean;
}): void {
  useEffect(() => {
    const host = hostRef.current;
    if (!host || !enabled) return;
    // Selecting is a reading gesture; drop the soft keyboard so the output the
    // user is trying to copy is actually on screen.
    terminalManager.blur(sessionId);
    const unbind = bindTerminalSelection(host, sessionId);
    return () => {
      unbind();
      terminalManager.clearSelection(sessionId);
    };
  }, [sessionId, hostRef, enabled]);
}
