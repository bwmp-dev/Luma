import { useEffect } from "react";
import { terminalManager } from "../terminal/terminalManager";

const FALLBACK_LINE_HEIGHT_PX = 16;

function midpointY(touches: TouchList): number | null {
  if (touches.length !== 2) return null;
  return (touches[0].clientY + touches[1].clientY) / 2;
}

/**
 * Turns a two-finger drag over the terminal into scrollback movement. Using the
 * midpoint makes a pinch (fingers moving in opposite directions) cancel out.
 */
export function bindTerminalScroll(host: HTMLElement, sessionId: string): () => void {
  let previousY: number | null = null;
  let remainingPixels = 0;
  let lineHeight = FALLBACK_LINE_HEIGHT_PX;

  const reset = () => {
    previousY = null;
    remainingPixels = 0;
  };

  const consume = (event: TouchEvent) => {
    event.preventDefault();
    event.stopPropagation();
  };

  const onTouchStart = (event: TouchEvent) => {
    const y = midpointY(event.touches);
    if (y === null) {
      if (event.touches.length > 2) reset();
      return;
    }
    previousY = y;
    remainingPixels = 0;
    lineHeight = terminalManager.cellHeight(sessionId) ?? FALLBACK_LINE_HEIGHT_PX;
    consume(event);
  };

  const onTouchMove = (event: TouchEvent) => {
    if (previousY === null) return;
    const y = midpointY(event.touches);
    if (y === null) {
      reset();
      return;
    }

    remainingPixels += y - previousY;
    previousY = y;
    const lines = Math.trunc(-remainingPixels / lineHeight);
    if (lines !== 0) {
      terminalManager.scrollLines(sessionId, lines);
      remainingPixels += lines * lineHeight;
    }
    consume(event);
  };

  const onTouchEnd = (event: TouchEvent) => {
    if (previousY === null) return;
    reset();
    consume(event);
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
}: {
  sessionId: string;
  hostRef: React.RefObject<HTMLDivElement | null>;
  enabled: boolean;
}): void {
  useEffect(() => {
    const host = hostRef.current;
    if (!host || !enabled) return;
    return bindTerminalScroll(host, sessionId);
  }, [sessionId, hostRef, enabled]);
}
