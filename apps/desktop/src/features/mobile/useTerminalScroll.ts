import { useEffect } from "react";
import { terminalManager } from "../terminal/terminalManager";

/*
 * Touch scrolling for the mobile terminal.
 *
 * xterm 6 drives its viewport from a VS Code scrollable element and has no touch
 * handling of its own: a finger dragged over the output moves nothing, and the
 * only thing that answers is the webview's own scroll view, which rubber-bands
 * the page (flashing iOS's scroll indicator) while the terminal stays put. So
 * the gesture is implemented here, against the buffer rather than against a
 * scroller, and `touch-action: none` on the host (see PaneView) keeps WebKit
 * from claiming the touches before these listeners ever see them.
 *
 * One finger scrolls, which is what every terminal on a phone does. Two fingers
 * scroll as well and are tracked by the mean of the touches, so a pinch (fingers
 * moving in opposite directions) cancels out instead of scrolling.
 */

const FALLBACK_LINE_HEIGHT_PX = 16;
/** How far the fingers travel before a drag is a scroll. Below this a single
 * touch may still become a tap or the long press that opens the arrow pad
 * (terminalGestures.ts), so nothing is consumed until it is cleared. */
const SCROLL_SLOP_PX = 8;

/** Mean Y of the active touches, or null when none are left. */
function meanY(touches: TouchList): number | null {
  if (touches.length === 0) return null;
  let total = 0;
  for (let index = 0; index < touches.length; index += 1) {
    total += touches[index].clientY;
  }
  return total / touches.length;
}

export type TerminalScrollOptions = {
  /** Let a single finger scroll. Off while another recognizer owns one-finger
   * drags: selection mode drags out a selection, and an open arrow pad is
   * already scrubbing history with the same movement. Two fingers always
   * scroll — neither of those claims them. */
  oneFinger: boolean;
};

/**
 * Attach the scroll listeners to `host`, the element the terminal is attached
 * into.
 * @returns a teardown that removes them.
 */
export function bindTerminalScroll(
  host: HTMLElement,
  sessionId: string,
  options: TerminalScrollOptions,
): () => void {
  /** Mean Y of the last event, or null when no touch is being tracked. */
  let previousY: number | null = null;
  /** Movement accumulated before the slop was cleared. */
  let travel = 0;
  /** True once this touch is a scroll and the events belong to us. */
  let engaged = false;
  /** Sub-row remainder, so a slow drag still scrolls once it adds up. */
  let remainingPixels = 0;
  let lineHeight = FALLBACK_LINE_HEIGHT_PX;

  const reset = () => {
    previousY = null;
    travel = 0;
    engaged = false;
    remainingPixels = 0;
  };

  const consume = (event: TouchEvent) => {
    event.preventDefault();
    // Capture phase: this also keeps the drag away from xterm's own handlers.
    event.stopPropagation();
  };

  /** Whether this many fingers may scroll at all. */
  const eligible = (count: number) => count > 1 || options.oneFinger;

  const onTouchStart = (event: TouchEvent) => {
    const y = meanY(event.touches);
    if (y === null) return;
    if (!eligible(event.touches.length)) {
      reset();
      return;
    }
    // A finger landing moves the mean; re-anchor rather than reading that jump
    // as a drag. Everything else is only reset for a genuinely new gesture.
    previousY = y;
    if (engaged) return;
    travel = 0;
    remainingPixels = 0;
    lineHeight = terminalManager.cellHeight(sessionId) ?? FALLBACK_LINE_HEIGHT_PX;
  };

  const onTouchMove = (event: TouchEvent) => {
    if (previousY === null) return;
    const y = meanY(event.touches);
    if (y === null || !eligible(event.touches.length)) {
      reset();
      return;
    }
    const delta = y - previousY;
    previousY = y;
    if (!engaged) {
      travel += Math.abs(delta);
      if (travel < SCROLL_SLOP_PX) return;
      engaged = true;
    }
    remainingPixels += delta;
    // Dragging down (positive delta) reveals older output, so the buffer moves
    // back — the content follows the finger.
    const lines = Math.trunc(-remainingPixels / lineHeight);
    if (lines !== 0) {
      terminalManager.scrollLines(sessionId, lines);
      remainingPixels += lines * lineHeight;
    }
    consume(event);
  };

  const onTouchEnd = (event: TouchEvent) => {
    const remaining = meanY(event.touches);
    if (remaining !== null) {
      // One of several fingers lifted: carry on from what is still down, unless
      // what is left is not ours to scroll with.
      if (previousY !== null) {
        previousY = eligible(event.touches.length) ? remaining : null;
      }
      return;
    }
    // Last finger up, so the gesture is over however it went. Consuming the
    // release suppresses the click it would otherwise synthesize, which xterm
    // would read as a tap on the row the drag ended over.
    if (engaged) consume(event);
    reset();
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
  };
}

export function useTerminalScroll({
  sessionId,
  hostRef,
  enabled,
  oneFinger,
}: {
  sessionId: string;
  hostRef: React.RefObject<HTMLDivElement | null>;
  enabled: boolean;
  oneFinger: boolean;
}): void {
  useEffect(() => {
    const host = hostRef.current;
    if (!host || !enabled) return;
    return bindTerminalScroll(host, sessionId, { oneFinger });
  }, [sessionId, hostRef, enabled, oneFinger]);
}
