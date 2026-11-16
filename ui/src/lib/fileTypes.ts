/**
 * Human type names and icon categories by extension.
 *
 * The listing carries a precomputed lowercase `ext`, so this is a table lookup per
 * visible row — no name parsing.
 */
import type { FileEntry } from "../api/types";

export type IconKind =
  | "folder"
  | "image"
  | "video"
  | "audio"
  | "archive"
  | "code"
  | "doc"
  | "sheet"
  | "slides"
  | "pdf"
  | "text"
  | "exe"
  | "link"
  | "file";

interface TypeInfo {
  icon: IconKind;
  label: string;
}

const TABLE: Record<string, TypeInfo> = {};
function reg(icon: IconKind, label: string, exts: string[]) {
  for (const e of exts) TABLE[e] = { icon, label: label.replace("%", e.toUpperCase()) };
}
reg("image", "% image", ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "heic", "avif", "ico"]);
reg("image", "SVG image", ["svg"]);
reg("video", "% video", ["mp4", "mkv", "mov", "avi", "webm", "wmv", "m4v"]);
reg("audio", "% audio", ["mp3", "wav", "flac", "ogg", "m4a", "aac", "opus"]);
reg("archive", "% archive", ["zip", "7z", "rar", "tar", "gz", "xz", "bz2", "zst", "iso"]);
reg("code", "% source", ["rs", "ts", "tsx", "js", "jsx", "py", "c", "h", "cpp", "hpp", "cs", "go", "java", "kt", "swift", "rb", "php", "sh", "ps1", "bat", "toml", "yaml", "yml", "json", "xml", "html", "css", "sql", "lua"]);
reg("doc", "Word document", ["doc", "docx", "odt", "rtf"]);
reg("sheet", "Spreadsheet", ["xls", "xlsx", "ods", "csv", "tsv"]);
reg("slides", "Presentation", ["ppt", "pptx", "odp", "key"]);
reg("pdf", "PDF document", ["pdf"]);
reg("text", "Text document", ["txt", "log", "ini", "cfg", "conf"]);
reg("text", "Markdown", ["md", "markdown"]);
reg("exe", "Application", ["exe", "msi", "app", "dll", "sys"]);
reg("link", "Shortcut", ["lnk", "url"]);

export function typeInfo(entry: Pick<FileEntry, "kind" | "ext">): TypeInfo {
  if (entry.kind === "Dir") return { icon: "folder", label: "Folder" };
  if (entry.kind === "Volume") return { icon: "folder", label: "Drive" };
  if (entry.kind === "Symlink") return { icon: "link", label: "Symbolic link" };
  if (!entry.ext) return { icon: "file", label: "File" };
  return TABLE[entry.ext] ?? { icon: "file", label: `${entry.ext.toUpperCase()} file` };
}

export type PreviewKind = "image" | "text" | "pdf" | "media" | "none";

/** What the preview pane can show for this entry, decided without touching the file. */
export function previewKind(entry: Pick<FileEntry, "kind" | "ext">): PreviewKind {
  if (entry.kind !== "File" || !entry.ext) return "none";
  const icon = TABLE[entry.ext]?.icon;
  if (icon === "image") return "image";
  if (icon === "pdf") return "pdf";
  if (icon === "video" || icon === "audio") return "media";
  if (icon === "text" || icon === "code") return "text";
  return "none";
}
