import { useEffect, useRef } from "react";
import { terminalManager } from "../terminal/terminalManager";
import { useTerminalStyleStore } from "../../stores/terminalStyleStore";
import { cn } from "../../lib/utils";

/*
 * A live, read-only window onto an open session, shown on its Connections card.
 *
 * There is no miniature and no mirror: terminalManager.previewSession parks the
 * session's OWN terminal in this box, scaled down by a CSS transform and
 * anchored to the bottom, so the card shows the real thing — the same buffer,
 * grid, theme, WebGL renderer and glyphs as the full-screen session, just
 * smaller and cropped to its last rows. Nothing here can diverge from the
 * terminal because there is nothing here but the terminal.
 *
 * That works because the mobile shell never renders a session in two places at
 * once (MobileLayout swaps the full-screen view and the list), and React runs
 * this component's cleanup before the full-screen pane's effect claims the
 * element back. This component only owns the box and the lifetime; no terminal
 * bytes pass through React.
 */

/** fitPreview measures the rendering currently on screen, so a pass taken before
 * the webfont has landed can be slightly off. Re-running it converges within a
 * pass or two; this only bounds the loop so a pathological case (metrics that
 * never settle) cannot spin every frame forever. */
const MAX_FIT_PASSES = 4;

export function MobileTerminalPreview({
  sessionId,
  status,
  className,
}: {
  sessionId: string;
  /** The session's status. Used only as an effect dependency: a session is
   * listed while it is still connecting, before the manager has a terminal to
   * park here, so the preview has to be retried once it does. */
  status: string;
  className?: string;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  // The card is taller and wider than the rows it can show, so the box has to
  // carry the terminal's own background or the leftover reads as a frame around
  // the output instead of as part of it. Subscribed rather than read once: the
  // manager owns the resolved theme, but only the store re-renders when the user
  // picks a different scheme.
  useTerminalStyleStore((state) => state.schemeId);
  const background = terminalManager.terminalBackground();

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let frame: number | null = null;
    // Each pass measures what is actually rendered and applies one correction,
    // so passes are chained across frames — a transform only shows up in the
    // next frame's metrics — until fitPreview reports the scale unchanged.
    const scheduleFit = (pass = 0) => {
      if (frame !== null) cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        frame = null;
        const before = terminalManager.previewScale(sessionId);
        const after = terminalManager.fitPreview(sessionId);
        if (after !== null && after !== before && pass + 1 < MAX_FIT_PASSES) {
          scheduleFit(pass + 1);
        }
      });
    };

    const release = terminalManager.previewSession(sessionId, host, {
      // What the card shows moved under it — new output, or an Appearance font
      // change — while the card's own box did not, so nothing else here would
      // prompt a refit. Coalesced by scheduleFit to at most one pass a frame.
      onChange: () => scheduleFit(),
    });
    // The card's width is known only after layout, and the bundled font can still
    // be loading (which changes cell metrics), so fit on every box change rather
    // than once at mount.
    // Wrapped rather than passed directly: ResizeObserver hands its callback an
    // entry list, which would arrive as scheduleFit's pass counter.
    const observer = new ResizeObserver(() => scheduleFit());
    observer.observe(host);
    scheduleFit();

    return () => {
      if (frame !== null) cancelAnimationFrame(frame);
      observer.disconnect();
      release();
    };
  }, [sessionId, status]);

  return (
    <div
      ref={hostRef}
      aria-hidden="true"
      // The preview is decorative: the card's own button carries the label and
      // the tap target, so it must not swallow touches meant for it.
      style={{ background }}
      className={cn(
        // `relative` is the containing block the parked element is positioned
        // in, and the clip that turns an oversized grid into its last rows.
        "luma-terminal-preview pointer-events-none relative overflow-hidden",
        className,
      )}
    />
  );
}
