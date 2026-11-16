/**
 * A minimal external store for `useSyncExternalStore`.
 *
 * App state lives outside React so the keybinding handlers, the job runner and the
 * components all see one source of truth, and so a component re-renders only when the
 * slice it selects actually changes — the property that keeps a 120k-row pane from
 * re-rendering when an unrelated toast ticks.
 */
import { useCallback, useRef, useSyncExternalStore } from "react";

export interface Store<T> {
  get(): T;
  set(next: T | ((prev: T) => T)): void;
  subscribe(listener: () => void): () => void;
}

export function createStore<T>(initial: T): Store<T> {
  let state = initial;
  const listeners = new Set<() => void>();
  return {
    get: () => state,
    set(next) {
      const value = typeof next === "function" ? (next as (p: T) => T)(state) : next;
      if (Object.is(value, state)) return;
      state = value;
      listeners.forEach((l) => l());
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

/**
 * Subscribes to a selected slice. The selector result is compared with `Object.is`, so
 * selectors must return existing references (or primitives), not fresh objects.
 */
export function useStore<T, S>(store: Store<T>, selector: (state: T) => S): S {
  const sel = useRef(selector);
  sel.current = selector;
  const get = useCallback(() => sel.current(store.get()), [store]);
  return useSyncExternalStore(store.subscribe, get, get);
}

/** Coalesces bursts of notifications (a 500-row batch every millisecond) into one per frame. */
export function frameScheduler(fn: () => void): () => void {
  let pending = false;
  const raf =
    typeof requestAnimationFrame === "function" ? requestAnimationFrame : (cb: () => void) => setTimeout(cb, 16);
  return () => {
    if (pending) return;
    pending = true;
    raf(() => {
      pending = false;
      fn();
    });
  };
}
