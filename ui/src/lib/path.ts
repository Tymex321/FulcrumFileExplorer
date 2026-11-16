/**
 * Path helpers for Windows-style paths, with POSIX tolerated so the mock and macOS dev
 * builds behave. All pure: these sit under navigation, drag and drop and the breadcrumb,
 * where an off-by-one separator turns into "C:Users" or a doubled backslash.
 */

/** The separator a path already uses; Windows wins when there is no evidence either way. */
export function separatorOf(path: string): "\\" | "/" {
  if (path.includes("\\")) return "\\";
  if (path.startsWith("/")) return "/";
  return "\\";
}

/** True for a drive root such as `C:\`, `C:` or a POSIX `/`. */
export function isRoot(path: string): boolean {
  return /^[A-Za-z]:[\\/]?$/.test(path) || path === "/" || /^\\\\[^\\]+\\[^\\]+\\?$/.test(path);
}

/**
 * Canonical form used as a key: no trailing separator except on a drive root, which
 * keeps its backslash (`C:` on its own means "current directory on C", not the root).
 */
export function normalize(path: string): string {
  let p = path.trim();
  if (!p) return p;
  if (/^[A-Za-z]:$/.test(p)) return `${p.toUpperCase()}\\`;
  if (/^[A-Za-z]:[\\/]/.test(p)) p = p[0]!.toUpperCase() + p.slice(1);
  const sep = separatorOf(p);
  const unc = p.startsWith("\\\\");
  // Collapse runs of separators (but not the UNC prefix) and unify direction.
  const body = unc ? p.slice(2) : p;
  let out = body.replace(/[\\/]+/g, sep);
  if (unc) out = `\\\\${out}`;
  if (out.length > 1 && out.endsWith(sep) && !isRoot(out)) out = out.slice(0, -1);
  return out;
}

export function join(dir: string, name: string): string {
  const sep = separatorOf(dir);
  return dir.endsWith("\\") || dir.endsWith("/") ? `${dir}${name}` : `${dir}${sep}${name}`;
}

/** The containing directory, or `null` at a root. */
export function parent(path: string): string | null {
  const p = normalize(path);
  if (!p || isRoot(p)) return null;
  const idx = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  if (idx < 0) return null;
  const head = p.slice(0, idx);
  if (/^[A-Za-z]:$/.test(head)) return `${head}\\`;
  if (head === "") return "/";
  if (head === "\\") return null; // `\\server` with no share: there is no parent to list.
  return head;
}

/** The last component; a drive root names itself (`C:`). */
export function basename(path: string): string {
  const p = normalize(path);
  if (/^[A-Za-z]:\\$/.test(p)) return p.slice(0, 2);
  if (p === "/") return "/";
  const idx = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  return idx < 0 ? p : p.slice(idx + 1);
}

/** Volume id (`C`) of a drive path, or `null` for UNC and POSIX paths. */
export function volumeOf(path: string): string | null {
  const m = /^([A-Za-z]):/.exec(path);
  return m ? m[1]!.toUpperCase() : null;
}

/** True when `child` is `ancestor` or lies beneath it. Case-insensitive, as NTFS is. */
export function isWithin(child: string, ancestor: string): boolean {
  const c = normalize(child).toLowerCase();
  const a = normalize(ancestor).toLowerCase();
  if (c === a) return true;
  const withSep = a.endsWith("\\") || a.endsWith("/") ? a : a + separatorOf(a);
  return c.startsWith(withSep);
}

/**
 * Picks a name that does not collide with `taken`, the way Explorer does:
 * `New folder`, `New folder (2)`, `New folder (3)`…
 */
export function uniqueName(base: string, taken: ReadonlySet<string>): string {
  const lower = new Set([...taken].map((t) => t.toLowerCase()));
  if (!lower.has(base.toLowerCase())) return base;
  const dot = base.lastIndexOf(".");
  const stem = dot > 0 ? base.slice(0, dot) : base;
  const ext = dot > 0 ? base.slice(dot) : "";
  for (let n = 2; ; n++) {
    const candidate = `${stem} (${n})${ext}`;
    if (!lower.has(candidate.toLowerCase())) return candidate;
  }
}

/**
 * Characters Windows rejects in a file name, plus the reserved device names. Returns a
 * reason string for the inline rename field, or `null` when the name is acceptable.
 */
export function invalidNameReason(name: string): string | null {
  if (!name.trim()) return "A name cannot be empty.";
  if (/[<>:"/\\|?*\u0000-\u001f]/.test(name)) return 'A name cannot contain < > : " / \\ | ? *';
  if (/[. ]$/.test(name)) return "A name cannot end with a space or a period.";
  if (/^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\..*)?$/i.test(name)) return `"${name}" is reserved by Windows.`;
  return null;
}

/** Splits a name into stem and extension for rename pre-selection. */
export function stemLength(name: string, isDir: boolean): number {
  if (isDir) return name.length;
  const dot = name.lastIndexOf(".");
  return dot > 0 ? dot : name.length;
}
