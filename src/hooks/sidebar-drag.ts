import React, {
  RefObject,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { SectionKind, SectionRef, sortKeyOf } from "@/utils/instance-groups";

// Pointer events instead of HTML5 drag and drop: the main window enables
// Tauri's file drop handler, under which WebView2 does not deliver drag events.

const DRAG_THRESHOLD_PX = 5;
const SCROLL_EDGE_PX = 40;
const SCROLL_MAX_STEP_PX = 16;

export type DragPayload =
  | { type: "instance"; instanceId: string; source: SectionRef; label: string }
  | { type: "section"; section: SectionRef; label: string };

export type DropTargetView =
  | { type: "section"; section: SectionRef }
  | { type: "new" }
  | { type: "blank" }
  | { type: "none" };

export interface SectionIndicator {
  sortKey: string;
  after: boolean;
  top: number;
  left: number;
  width: number;
}

export interface DragView {
  payload: DragPayload;
  x: number;
  y: number;
  copy: boolean;
  target: DropTargetView;
  indicator: SectionIndicator | null;
}

export interface InstanceDrop {
  instanceId: string;
  source: SectionRef;
  /** "new" is the plus card; null is a spot in the sidebar outside any section. */
  target: SectionRef | "new" | null;
  copy: boolean;
}

export interface SectionMove {
  key: string;
  targetKey: string;
  after: boolean;
}

interface Options {
  scrollRef: RefObject<HTMLElement>;
  onDropInstance: (drop: InstanceDrop) => void;
  onMoveSection: (move: SectionMove) => void;
}

interface Session {
  payload: DragPayload;
  startX: number;
  startY: number;
  x: number;
  y: number;
  shift: boolean;
  active: boolean;
  raf: number;
  cleanup: () => void;
}

const contains = (rect: DOMRect, x: number, y: number) =>
  x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;

const sectionRef = (element: HTMLElement): SectionRef => ({
  key: element.dataset.sectionKey ?? "",
  kind: (element.dataset.sectionKind ?? "custom") as SectionKind,
});

export const useSidebarDrag = ({
  scrollRef,
  onDropInstance,
  onMoveSection,
}: Options) => {
  const [drag, setDrag] = useState<DragView | null>(null);
  const session = useRef<Session | null>(null);
  const handlers = useRef({ onDropInstance, onMoveSection });
  handlers.current = { onDropInstance, onMoveSection };

  const locate = useCallback(
    (
      payload: DragPayload,
      x: number,
      y: number
    ): { target: DropTargetView; indicator: SectionIndicator | null } => {
      const scroll = scrollRef.current;
      const none = {
        target: { type: "none" } as DropTargetView,
        indicator: null,
      };
      if (!scroll) return none;
      const scrollRect = scroll.getBoundingClientRect();
      const elements = Array.from(
        scroll.querySelectorAll<HTMLElement>("[data-section-key]")
      );

      if (payload.type === "instance") {
        if (contains(scrollRect, x, y)) {
          const plus = scroll.querySelector<HTMLElement>("[data-sidebar-plus]");
          if (plus && contains(plus.getBoundingClientRect(), x, y)) {
            return { target: { type: "new" }, indicator: null };
          }
          const hit = elements.find((element) =>
            contains(element.getBoundingClientRect(), x, y)
          );
          if (hit) {
            return {
              target: { type: "section", section: sectionRef(hit) },
              indicator: null,
            };
          }
          return { target: { type: "blank" }, indicator: null };
        }
        // Anywhere in the window outside the sections takes it out.
        return { target: { type: "blank" }, indicator: null };
      }

      // Sections: directory sections share one slot, measured as a block.
      if (!contains(scrollRect, x, y)) return none;
      const blocks = new Map<
        string,
        { sortKey: string; top: number; bottom: number }
      >();
      for (const element of elements) {
        if (element.dataset.sortable !== "true") continue;
        const rect = element.getBoundingClientRect();
        const sortKey = sortKeyOf(element.dataset.sectionKey ?? "");
        const block = blocks.get(sortKey);
        blocks.set(sortKey, {
          sortKey,
          top: Math.min(block?.top ?? rect.top, rect.top),
          bottom: Math.max(block?.bottom ?? rect.bottom, rect.bottom),
        });
      }
      const ordered = Array.from(blocks.values()).sort((a, b) => a.top - b.top);
      if (ordered.length === 0) return none;
      let chosen = ordered.find((block) => y >= block.top && y <= block.bottom);
      let after: boolean;
      if (chosen) after = y > (chosen.top + chosen.bottom) / 2;
      else if (y < ordered[0].top) {
        chosen = ordered[0];
        after = false;
      } else if (y > ordered[ordered.length - 1].bottom) {
        chosen = ordered[ordered.length - 1];
        after = true;
      } else {
        // in a gap or over a fixed section: the nearest block by centre
        chosen = ordered.reduce((best, block) =>
          Math.abs(y - (block.top + block.bottom) / 2) <
          Math.abs(y - (best.top + best.bottom) / 2)
            ? block
            : best
        );
        after = y > (chosen.top + chosen.bottom) / 2;
      }
      if (chosen.sortKey === sortKeyOf(payload.section.key)) return none;
      return {
        target: { type: "none" },
        indicator: {
          sortKey: chosen.sortKey,
          after,
          top: after ? chosen.bottom : chosen.top,
          left: scrollRect.left,
          width: scrollRect.width,
        },
      };
    },
    [scrollRef]
  );

  const publish = useCallback(() => {
    const s = session.current;
    if (!s?.active) return;
    setDrag({
      payload: s.payload,
      x: s.x,
      y: s.y,
      copy: s.shift,
      ...locate(s.payload, s.x, s.y),
    });
  }, [locate]);

  const stop = useCallback(() => {
    const s = session.current;
    if (!s) return;
    session.current = null;
    s.cleanup();
    setDrag(null);
  }, []);

  useEffect(() => stop, [stop]);

  const startDrag = useCallback(
    (event: React.PointerEvent, payload: DragPayload) => {
      if (event.button !== 0 || !event.isPrimary || session.current) return;
      const s: Session = {
        payload,
        startX: event.clientX,
        startY: event.clientY,
        x: event.clientX,
        y: event.clientY,
        shift: event.shiftKey,
        active: false,
        raf: 0,
        cleanup: () => {},
      };
      session.current = s;

      const scrollStep = () => {
        const scroll = scrollRef.current;
        if (!session.current || !scroll) return;
        const rect = scroll.getBoundingClientRect();
        if (s.x >= rect.left && s.x <= rect.right) {
          const speed = (depth: number) =>
            Math.ceil(Math.min(1, depth / SCROLL_EDGE_PX) * SCROLL_MAX_STEP_PX);
          let step = 0;
          if (s.y < rect.top + SCROLL_EDGE_PX) {
            step = -speed(rect.top + SCROLL_EDGE_PX - s.y);
          } else if (s.y > rect.bottom - SCROLL_EDGE_PX) {
            step = speed(s.y - (rect.bottom - SCROLL_EDGE_PX));
          }
          const before = scroll.scrollTop;
          if (step !== 0) scroll.scrollTop = before + step;
          if (scroll.scrollTop !== before) publish();
        }
        s.raf = requestAnimationFrame(scrollStep);
      };

      // The click that follows a finished or cancelled drag must not navigate.
      const swallowClick = (inPointerUp: boolean) => {
        const block = (clickEvent: MouseEvent) => {
          clickEvent.stopPropagation();
          clickEvent.preventDefault();
        };
        window.addEventListener("click", block, true);
        const release = () =>
          window.setTimeout(
            () => window.removeEventListener("click", block, true),
            0
          );
        if (inPointerUp) release();
        else {
          // cancelled with Esc while the button is still down: wait for the release
          window.addEventListener("pointerup", release, { once: true });
          window.addEventListener("pointerdown", release, {
            once: true,
            capture: true,
          });
        }
      };

      const activate = () => {
        s.active = true;
        document.body.style.userSelect = "none";
        document.body.style.cursor = "grabbing";
        s.raf = requestAnimationFrame(scrollStep);
      };

      const onMove = (e: PointerEvent) => {
        s.x = e.clientX;
        s.y = e.clientY;
        s.shift = e.shiftKey;
        if (!s.active) {
          if (Math.hypot(s.x - s.startX, s.y - s.startY) < DRAG_THRESHOLD_PX) {
            return;
          }
          activate();
        }
        if (e.buttons === 0) {
          // the release happened outside the window
          stop();
          return;
        }
        publish();
      };

      const onUp = (e: PointerEvent) => {
        const wasActive = s.active;
        s.x = e.clientX;
        s.y = e.clientY;
        // Shift is read at release, not at press.
        const copy = e.shiftKey;
        const { target, indicator } = wasActive
          ? locate(s.payload, s.x, s.y)
          : { target: { type: "none" } as DropTargetView, indicator: null };
        stop();
        if (!wasActive) return;
        swallowClick(true);
        if (s.payload.type === "instance") {
          const { instanceId, source } = s.payload;
          if (target.type === "section") {
            handlers.current.onDropInstance({
              instanceId,
              source,
              target: target.section,
              copy,
            });
          } else if (target.type === "new") {
            handlers.current.onDropInstance({
              instanceId,
              source,
              target: "new",
              copy,
            });
          } else if (target.type === "blank") {
            handlers.current.onDropInstance({
              instanceId,
              source,
              target: null,
              copy,
            });
          }
        } else if (indicator) {
          handlers.current.onMoveSection({
            key: s.payload.section.key,
            targetKey: indicator.sortKey,
            after: indicator.after,
          });
        }
      };

      const onKey = (e: KeyboardEvent) => {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          const wasActive = s.active;
          stop();
          if (wasActive) swallowClick(false);
        } else if (e.key === "Shift" && s.active) {
          s.shift = e.type === "keydown";
          publish();
        }
      };

      const onCancel = () => stop();

      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
      window.addEventListener("pointercancel", onCancel);
      window.addEventListener("blur", onCancel);
      window.addEventListener("keydown", onKey, true);
      window.addEventListener("keyup", onKey, true);
      s.cleanup = () => {
        cancelAnimationFrame(s.raf);
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
        window.removeEventListener("pointercancel", onCancel);
        window.removeEventListener("blur", onCancel);
        window.removeEventListener("keydown", onKey, true);
        window.removeEventListener("keyup", onKey, true);
        document.body.style.userSelect = "";
        document.body.style.cursor = "";
      };
    },
    [locate, publish, scrollRef, stop]
  );

  return { drag, startDrag };
};
