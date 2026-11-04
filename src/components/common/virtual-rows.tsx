import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { AutoSizer, List, ListRowProps } from "react-virtualized";

/** Rows no shorter than this let a long list skip summing every height. */
const MIN_ROW_HEIGHT = 12;

const isTextField = (target: HTMLElement) =>
  target.tagName === "TEXTAREA" ||
  target.tagName === "SELECT" ||
  (target.tagName === "INPUT" &&
    !["checkbox", "radio", "button"].includes(
      (target as HTMLInputElement).type
    ));

const SCROLLABLE = /(auto|scroll|overlay)/;

/** The nearest ancestor that scrolls vertically. */
const findScrollParent = (from: HTMLElement | null): HTMLElement | null => {
  let element = from?.parentElement ?? null;
  while (element && element !== document.body) {
    if (SCROLLABLE.test(getComputedStyle(element).overflowY)) return element;
    element = element.parentElement;
  }
  return null;
};

/** Elements that stay at the top of a scroller (`data-sticky-top`) cover its first rows. */
const stickyInset = (scroller: HTMLElement): number =>
  Array.from(
    scroller.querySelectorAll<HTMLElement>("[data-sticky-top]")
  ).reduce((sum, element) => sum + element.getBoundingClientRect().height, 0);

interface ParentView {
  scrollTop: number;
  height: number;
  /** Where the list starts inside the scroller's content, in px. */
  offset: number;
}

/**
 * Follows the ancestor that scrolls the list: its scroll position and height,
 * and where the list sits inside it. The list is re-measured after every
 * render, and when the scroller or what it holds changes size, because the
 * content above the list may grow or shrink.
 */
const useParentView = (
  boxRef: React.RefObject<HTMLElement>,
  enabled: boolean
) => {
  const [scroller, setScroller] = useState<HTMLElement | null>(null);
  const [view, setView] = useState<ParentView>({
    scrollTop: 0,
    height: 0,
    offset: 0,
  });
  const viewRef = useRef(view);

  useLayoutEffect(() => {
    if (enabled) setScroller(findScrollParent(boxRef.current));
  }, [enabled, boxRef]);

  const measure = useCallback(() => {
    const box = boxRef.current;
    if (!scroller || !box) return;
    const offset =
      box.getBoundingClientRect().top -
      scroller.getBoundingClientRect().top +
      scroller.scrollTop -
      scroller.clientTop;
    const previous = viewRef.current;
    if (
      previous.scrollTop === scroller.scrollTop &&
      previous.height === scroller.clientHeight &&
      Math.abs(previous.offset - offset) < 0.5
    ) {
      return;
    }
    const next = {
      scrollTop: scroller.scrollTop,
      height: scroller.clientHeight,
      offset,
    };
    viewRef.current = next;
    setView(next);
  }, [scroller, boxRef]);

  useLayoutEffect(measure);

  useEffect(() => {
    if (!scroller) return;
    scroller.addEventListener("scroll", measure, { passive: true });
    const observer = new ResizeObserver(measure);
    observer.observe(scroller);
    Array.from(scroller.children).forEach((child) => observer.observe(child));
    return () => {
      scroller.removeEventListener("scroll", measure);
      observer.disconnect();
    };
  }, [scroller, measure]);

  return { scroller, view };
};

interface MeasuredRowProps {
  index: number;
  rowKey: string;
  style: React.CSSProperties;
  onHeight: (key: string, index: number, height: number) => void;
  children: React.ReactNode;
}

/**
 * Reports the natural height of its content, now and whenever it changes
 * (a card opens, a tag appears, the width wraps a line), so the list can place
 * the rows around it.
 */
const MeasuredRow: React.FC<MeasuredRowProps> = ({
  index,
  rowKey,
  style,
  onHeight,
  children,
}) => {
  const inner = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const element = inner.current;
    if (!element) return;
    const report = () =>
      onHeight(
        rowKey,
        index,
        Math.ceil(element.getBoundingClientRect().height)
      );
    report();
    const observer = new ResizeObserver(report);
    observer.observe(element);
    return () => observer.disconnect();
  }, [rowKey, index, onHeight]);
  return (
    <div style={style} data-row-index={index}>
      <div ref={inner}>{children}</div>
    </div>
  );
};

interface VirtualRowsProps {
  rowCount: number;
  /** A stable name for the row, so its measured height follows it when rows shift. */
  rowKey: (index: number) => string;
  renderRow: (index: number) => React.ReactNode;
  /** The most the list grows to, in px; it is shorter when its rows are. */
  maxHeight?: number;
  /**
   * Take the whole height of the rows and let the nearest scrolling ancestor
   * scroll them, drawing only the rows in its view. `maxHeight` is ignored.
   */
  scrollParent?: boolean;
  /** The height assumed for a row until it is shown and measured. */
  estimatedRowHeight?: number;
  overscan?: number;
  role?: string;
  rowRole?: string;
  ariaLabel?: string;
  /** Changes whenever the rows' order or number changes. */
  layoutKey?: unknown;
}

/**
 * A list that draws only the rows near its viewport. Rows may differ in height
 * and change it: each reports its own height, and the viewport is as tall as
 * the rows up to `maxHeight`, so a short list does not leave a gap.
 */
const VirtualRows: React.FC<VirtualRowsProps> = ({
  rowCount,
  rowKey,
  renderRow,
  maxHeight = 320,
  scrollParent = false,
  estimatedRowHeight = 28,
  overscan = 8,
  role = "list",
  rowRole = "presentation",
  ariaLabel,
  layoutKey,
}) => {
  const listRef = useRef<List>(null);
  const boxRef = useRef<HTMLDivElement>(null);
  const heights = useRef(new Map<string, number>());
  const keyRef = useRef(rowKey);
  keyRef.current = rowKey;
  const countRef = useRef(rowCount);
  countRef.current = rowCount;
  const pending = useRef<number | null>(null);
  const mounted = useRef(true);
  const [total, setTotal] = useState<number | null>(null);

  const sumHeights = useCallback(() => {
    // A long list is taller than any viewport whatever its rows measure.
    if (scrollParent || countRef.current * MIN_ROW_HEIGHT >= maxHeight) {
      return null;
    }
    let sum = 0;
    for (let i = 0; i < countRef.current; i += 1) {
      sum += heights.current.get(keyRef.current(i)) ?? estimatedRowHeight;
    }
    return sum;
  }, [maxHeight, scrollParent, estimatedRowHeight]);

  // The list needs measuring, so the static export draws an empty box.
  const [ready, setReady] = useState(false);
  useEffect(() => {
    mounted.current = true;
    setReady(true);
    return () => {
      mounted.current = false;
    };
  }, []);

  const flush = useCallback(() => {
    const from = pending.current;
    pending.current = null;
    if (from === null || !mounted.current) return;
    listRef.current?.recomputeRowHeights(from);
    setTotal(sumHeights());
  }, [sumHeights]);

  const handleHeight = useCallback(
    (key: string, index: number, height: number) => {
      const known = heights.current.get(key);
      if (known !== undefined && Math.abs(known - height) < 1) return;
      heights.current.set(key, height);
      const wasIdle = pending.current === null;
      pending.current = Math.min(pending.current ?? index, index);
      // Rows report while React commits; the list takes the heights right after.
      if (wasIdle) queueMicrotask(flush);
    },
    [flush]
  );

  useEffect(() => {
    listRef.current?.recomputeRowHeights(0);
    setTotal(sumHeights());
  }, [layoutKey, rowCount, sumHeights]);

  const viewport = Math.min(maxHeight, total ?? maxHeight);
  const { scroller, view } = useParentView(boxRef, scrollParent);

  /** Bring a row into the scroller's view, clear of what sticks to its top. */
  const scrollToRow = (index: number) => {
    if (!scroller) {
      listRef.current?.scrollToRow(index);
      return;
    }
    let top = view.offset;
    for (let i = 0; i < index; i += 1) {
      top += heights.current.get(rowKey(i)) ?? estimatedRowHeight;
    }
    const bottom =
      top + (heights.current.get(rowKey(index)) ?? estimatedRowHeight);
    const inset = stickyInset(scroller);
    if (top < scroller.scrollTop + inset) {
      scroller.scrollTop = top - inset;
    } else if (bottom > scroller.scrollTop + scroller.clientHeight) {
      scroller.scrollTop = bottom - scroller.clientHeight;
    }
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const target = event.target as HTMLElement;
    if (isTextField(target) || event.altKey || event.ctrlKey || event.metaKey) {
      return;
    }
    const row = target.closest<HTMLElement>("[data-row-index]");
    if (!row) return;
    const next =
      Number(row.dataset.rowIndex) + (event.key === "ArrowDown" ? 1 : -1);
    if (next < 0 || next >= rowCount) return;
    event.preventDefault();
    // The neighbor may not be drawn yet: scroll to it, then focus once it is.
    scrollToRow(next);
    let tries = 0;
    const focusRow = () => {
      const element = boxRef.current?.querySelector<HTMLElement>(
        `[data-row-index="${next}"]`
      );
      const focusable = element?.querySelector<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), [tabindex="0"]'
      );
      if (focusable) focusable.focus({ preventScroll: scrollParent });
      else if (tries < 5) {
        tries += 1;
        requestAnimationFrame(focusRow);
      }
    };
    requestAnimationFrame(focusRow);
  };

  const rowRenderer = ({ index, style }: ListRowProps) => (
    <MeasuredRow
      // Keep focus and hover-related state from passing to another row's controls.
      key={rowKey(index)}
      index={index}
      rowKey={rowKey(index)}
      style={style}
      onHeight={handleHeight}
    >
      {renderRow(index)}
    </MeasuredRow>
  );

  return (
    // The keys move focus between rows; the list itself is not a control.
    <div
      ref={boxRef}
      style={{ height: scrollParent ? undefined : viewport, width: "100%" }}
      onKeyDown={handleKeyDown}
    >
      <AutoSizer disableHeight>
        {({ width }) =>
          !ready || (scrollParent && !scroller) ? null : (
            <List
              ref={listRef}
              width={width}
              autoHeight={scrollParent}
              height={
                scrollParent
                  ? Math.max(view.height, estimatedRowHeight)
                  : viewport
              }
              scrollTop={
                scrollParent
                  ? Math.max(0, view.scrollTop - view.offset)
                  : undefined
              }
              rowCount={rowCount}
              rowHeight={({ index }) =>
                heights.current.get(rowKey(index)) ?? estimatedRowHeight
              }
              estimatedRowSize={estimatedRowHeight}
              overscanRowCount={overscan}
              rowRenderer={rowRenderer}
              role={role}
              containerRole={rowRole}
              aria-label={ariaLabel ?? ""}
              tabIndex={null}
              style={{ outline: "none" }}
            />
          )
        }
      </AutoSizer>
    </div>
  );
};

export default VirtualRows;
