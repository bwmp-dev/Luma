import { useLayoutEffect, useState, type RefObject } from "react";

/*
 * Fixed-height row windowing for the file browsers. A directory listing is the
 * one place in the app where the row count is set by the remote filesystem
 * rather than by anything the app controls, and rendering every row of a large
 * folder is what made those folders unusable: each row carries a context menu
 * and a dropdown, so the cost is far higher than the markup suggests.
 *
 * Rows are laid out at an exact pixel height (the callers set it as an inline
 * height, so the constant and the DOM cannot drift), which is what lets both
 * the window and lasso hit-testing be arithmetic instead of DOM measurement.
 */

export type VirtualWindow = {
  /** First rendered index. */
  start: number;
  /** One past the last rendered index. */
  end: number;
  /** Spacer heights that keep the scrollbar sized for the whole list. */
  padTop: number;
  padBottom: number;
};

export function useVirtualRows(
  count: number,
  rowHeight: number,
  scrollRef: RefObject<HTMLElement | null>,
  overscan = 12,
): VirtualWindow {
  const [range, setRange] = useState({ start: 0, end: 0 });

  // Layout effect, not effect: the first measure has to land before paint or
  // the list shows one empty frame every time it mounts or a folder loads.
  useLayoutEffect(() => {
    const node = scrollRef.current;
    if (!node) return;
    let frame = 0;

    const measure = () => {
      frame = 0;
      const first = Math.floor(node.scrollTop / rowHeight);
      const onScreen = Math.ceil(node.clientHeight / rowHeight);
      const start = Math.max(0, first - overscan);
      const end = Math.min(count, first + onScreen + overscan);
      setRange((prev) =>
        prev.start === start && prev.end === end ? prev : { start, end },
      );
    };
    // Scroll fires far faster than the list can usefully change, so coalesce to
    // one recompute per frame.
    const schedule = () => {
      if (frame === 0) frame = requestAnimationFrame(measure);
    };

    measure();
    node.addEventListener("scroll", schedule, { passive: true });
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedule);
    observer?.observe(node);
    return () => {
      if (frame !== 0) cancelAnimationFrame(frame);
      node.removeEventListener("scroll", schedule);
      observer?.disconnect();
    };
  }, [count, rowHeight, overscan, scrollRef]);

  // Clamp against a count that shrank since the last measure (a filter keystroke
  // or a navigation renders before the effect re-runs).
  const end = Math.min(range.end, count);
  const start = Math.min(range.start, end);
  return {
    start,
    end,
    padTop: start * rowHeight,
    padBottom: Math.max(0, (count - end) * rowHeight),
  };
}

/** Scroll `index` into view in a windowed list, arithmetically — the row may
 * not be mounted, so `scrollIntoView` is not available. */
export function scrollRowIntoView(
  node: HTMLElement,
  index: number,
  rowHeight: number,
): void {
  const top = index * rowHeight;
  const bottom = top + rowHeight;
  if (top < node.scrollTop) node.scrollTop = top;
  else if (bottom > node.scrollTop + node.clientHeight) {
    node.scrollTop = bottom - node.clientHeight;
  }
}

/** Inclusive index range of the rows a content-coordinate band covers. */
export function rowsInBand(
  top: number,
  bottom: number,
  rowHeight: number,
  count: number,
): { from: number; to: number } {
  const from = Math.max(0, Math.floor(top / rowHeight));
  const to = Math.min(count - 1, Math.ceil(bottom / rowHeight) - 1);
  return { from, to };
}

/** A stable empty listing, so `?? NO_ENTRIES` does not hand the memos a fresh
 * array on every render and invalidate the sort/filter work. */
export const NO_ENTRIES: never[] = [];
