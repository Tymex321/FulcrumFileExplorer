/**
 * Lazy thumbnail cache.
 *
 * Thumbnails are requested only for cells that have stayed on screen briefly, so a fast
 * flick through 120k photos does not queue 120k decode requests behind the ones the user
 * actually stopped on. Fallback icons are not cached as if they were content, per
 * `Thumbnail::is_fallback`.
 */
import { useEffect, useState } from "react";
import { getApi } from "../api";

const LIMIT = 600;
const cache = new Map<string, string | null>();
const inflight = new Map<string, Promise<string | null>>();

function remember(key: string, value: string | null) {
  cache.set(key, value);
  if (cache.size > LIMIT) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
}

function load(path: string, size: number): Promise<string | null> {
  const key = `${size}|${path}`;
  const existing = inflight.get(key);
  if (existing) return existing;
  const p = getApi()
    .then((api) => api.thumbnail(path, size))
    .then((t) => {
      const url = t.is_fallback || !t.data_base64 ? null : `data:image/png;base64,${t.data_base64}`;
      if (!t.is_fallback) remember(key, url);
      return url;
    })
    .catch(() => null)
    .finally(() => inflight.delete(key));
  inflight.set(key, p);
  return p;
}

/** Returns a data URL once loaded, `null` while loading or when there is none. */
export function useThumbnail(path: string | undefined, size: number, enabled: boolean, delayMs = 120): string | null {
  const key = path ? `${size}|${path}` : "";
  const [url, setUrl] = useState<string | null>(() => (key ? (cache.get(key) ?? null) : null));

  useEffect(() => {
    if (!path || !enabled) {
      setUrl(null);
      return;
    }
    const hit = cache.get(key);
    if (hit !== undefined) {
      setUrl(hit);
      return;
    }
    setUrl(null);
    let live = true;
    const timer = setTimeout(() => {
      void load(path, size).then((u) => live && setUrl(u));
    }, delayMs);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [key, path, size, enabled, delayMs]);

  return url;
}
