/**
 * Anchored popover and menu primitives.
 *
 * Closing on Escape goes through the keybinding registry (`ui.cancel`), so a popover on
 * top of a rename field on top of a search box closes in the right order — the most
 * recently opened layer handles Escape first.
 */
import { type ReactNode, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useCommand, useCommands } from "../state/commands";

export function usePopover() {
  const [anchor, setAnchor] = useState<DOMRect | null>(null);
  return {
    anchor,
    open: (el: HTMLElement) => setAnchor(el.getBoundingClientRect()),
    toggle: (el: HTMLElement) => setAnchor((a) => (a ? null : el.getBoundingClientRect())),
    close: () => setAnchor(null),
  };
}

interface PopoverProps {
  anchor: DOMRect | { x: number; y: number } | null;
  onClose: () => void;
  children: ReactNode;
  align?: "start" | "end";
  className?: string;
  width?: number;
}

/** Positions below an anchor rect (or at a point), flipping to stay on screen. */
export function Popover({ anchor, onClose, children, align = "start", className = "", width }: PopoverProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  useCommand("ui.cancel", () => onClose(), anchor !== null);

  useLayoutEffect(() => {
    if (!anchor || !ref.current) return;
    const el = ref.current;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    const isRect = "bottom" in anchor;
    let left = isRect ? (align === "end" ? anchor.right - w : anchor.left) : anchor.x;
    let top = isRect ? anchor.bottom + 4 : anchor.y;
    if (left + w > window.innerWidth - 8) left = Math.max(8, window.innerWidth - w - 8);
    if (top + h > window.innerHeight - 8) top = Math.max(8, (isRect ? anchor.top - 4 : anchor.y) - h);
    setPos({ left, top });
  }, [anchor, align]);

  useEffect(() => {
    if (!anchor) return;
    const down = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const blur = () => onClose();
    // Capture so the click that closes a popover is not also swallowed by it.
    window.addEventListener("mousedown", down, true);
    window.addEventListener("blur", blur);
    window.addEventListener("resize", blur);
    return () => {
      window.removeEventListener("mousedown", down, true);
      window.removeEventListener("blur", blur);
      window.removeEventListener("resize", blur);
    };
  }, [anchor, onClose]);

  if (!anchor) return null;
  return createPortal(
    <div
      ref={ref}
      className={`popover fixed z-50 p-1 ${className}`}
      style={{ left: pos?.left ?? -9999, top: pos?.top ?? -9999, width }}
      onContextMenu={(e) => e.preventDefault()}
    >
      {children}
    </div>,
    document.body,
  );
}

export interface MenuItem {
  id: string;
  label: string;
  icon?: ReactNode;
  hint?: ReactNode;
  disabled?: boolean;
  danger?: boolean;
  checked?: boolean;
  run: () => void;
}

export type MenuEntry = MenuItem | "separator" | { heading: string };

/**
 * A keyboard-navigable menu. Arrow keys and Enter are claimed through the registry
 * while it is open, so they do not also move the file list underneath.
 */
export function Menu({ items, onClose }: { items: MenuEntry[]; onClose: () => void }) {
  const actionable = items.map((it, i) => (typeof it === "object" && "run" in it && !it.disabled ? i : -1)).filter((i) => i >= 0);
  const [active, setActive] = useState<number>(-1);

  const step = (d: number) => {
    if (!actionable.length) return;
    const pos = actionable.indexOf(active);
    const next = pos < 0 ? (d > 0 ? 0 : actionable.length - 1) : (pos + d + actionable.length) % actionable.length;
    setActive(actionable[next]!);
  };
  const run = (i: number) => {
    const it = items[i];
    if (typeof it === "object" && "run" in it && !it.disabled) {
      onClose();
      it.run();
    }
  };

  useCommands({
    "list.down": () => step(1),
    "list.up": () => step(-1),
    "list.home": () => setActive(actionable[0] ?? -1),
    "list.end": () => setActive(actionable[actionable.length - 1] ?? -1),
    "list.open": () => (active >= 0 ? run(active) : false),
  });

  return (
    <div role="menu" className="min-w-52">
      {items.map((it, i) => {
        if (it === "separator") return <div key={i} className="mx-2 my-1 h-px bg-hairline" />;
        if ("heading" in it) return <div key={i} className="px-2.5 pb-1 pt-2 text-[10px] font-semibold uppercase tracking-wider text-ink-faint">{it.heading}</div>;
        return (
          <div
            key={it.id}
            role="menuitem"
            aria-disabled={it.disabled}
            data-active={active === i}
            className={`menu-item ${it.danger ? "text-danger" : ""}`}
            onMouseEnter={() => !it.disabled && setActive(i)}
            onMouseLeave={() => setActive(-1)}
            onClick={() => run(i)}
          >
            <span className="grid w-4 place-items-center text-ink-muted">{it.checked ? <CheckMark /> : it.icon}</span>
            <span className="flex-1 truncate">{it.label}</span>
            {it.hint && <span className="ml-4 text-[11px] text-ink-faint">{it.hint}</span>}
          </div>
        );
      })}
    </div>
  );
}

function CheckMark() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="var(--color-accent-300)" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="m3.5 8.5 3 3 6-7" />
    </svg>
  );
}
