/**
 * Measures a scroll container and tracks its scroll offset.
 *
 * Scroll position is the one piece of per-frame state in the list; it lives here, inside
 * the view component, so a scroll re-renders only the visible rows and nothing above.
 */
import { useLayoutEffect, useState, type RefObject } from "react";

export interface Viewport {
  width: number;
  height: number;
  scrollTop: number;
}

export function useViewport(ref: RefObject<HTMLElement | null>): Viewport {
  const [vp, setVp] = useState<Viewport>({ width: 0, height: 0, scrollTop: 0 });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () =>
      setVp((v) =>
        v.width === el.clientWidth && v.height === el.clientHeight && v.scrollTop === el.scrollTop
          ? v
          : { width: el.clientWidth, height: el.clientHeight, scrollTop: el.scrollTop },
      );
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    el.addEventListener("scroll", measure, { passive: true });
    measure();
    return () => {
      ro.disconnect();
      el.removeEventListener("scroll", measure);
    };
  }, [ref]);

  return vp;
}
