/**
 * Transfer toasts and the conflict dialog.
 *
 * Each operation gets a card with a real bar, speed and ETA — and when the backend does
 * not know yet (`Scanning`, or `eta_secs: null`) the card says so instead of guessing.
 * A conflict (`Progress::pending_conflict`) raises a dialog showing both files side by
 * side, because "Replace?" without sizes and dates is not a decision anyone can make.
 */
import { useLayoutEffect, useRef, useState } from "react";
import type { Conflict, ConflictPolicy, OpId } from "../api/types";
import { formatDate, formatSize } from "../lib/format";
import { detailLine, fraction, isTerminal, jobTitle } from "../lib/jobs";
import { basename, parent } from "../lib/path";
import { controlOp, dismissJob, type Job, useJobs } from "../state/app";
import { useCommand } from "../state/commands";
import { typeInfo } from "../lib/fileTypes";
import { FileIcon, I } from "./Icons";
import { Modal } from "./Overlays";

export function JobToasts() {
  const jobs = useJobs((j) => j);
  const conflict = jobs.find((j) => j.progress?.state === "Paused" && j.progress.pending_conflict);
  if (jobs.length === 0) return null;
  return (
    <>
      <div aria-live="polite" className="fixed bottom-10 right-4 z-40 flex w-[360px] flex-col gap-2">
        {jobs.slice(-4).map((j) => (
          <JobCard key={j.id} job={j} />
        ))}
        {jobs.length > 4 && <div className="text-right text-[11px] text-ink-faint">+{jobs.length - 4} more operations</div>}
      </div>
      {conflict?.progress?.pending_conflict && <ConflictDialog key={`${conflict.id}:${conflict.progress.pending_conflict.source}`} op={conflict.id} conflict={conflict.progress.pending_conflict} />}
    </>
  );
}

function JobCard({ job }: { job: Job }) {
  const p = job.progress;
  const frac = fraction(p);
  const done = isTerminal(p);
  const paused = p?.state === "Paused";
  const failed = p?.state === "Failed" || (p?.errors.length ?? 0) > 0;

  return (
    <div className="popover p-3" style={{ animation: "slide-up var(--dur) var(--ease-out)" }}>
      <div className="flex items-start gap-2.5">
        <div className={`mt-0.5 grid size-7 shrink-0 place-items-center rounded-md ${p?.state === "Done" ? "bg-[color-mix(in_oklab,var(--color-ok)_14%,transparent)] text-ok" : "bg-select text-accent-300"}`}>
          {p?.state === "Done" ? <I.Check size={14} /> : job.request.kind === "Trash" || job.request.kind === "Delete" ? <I.Trash size={14} /> : job.request.kind === "Move" ? <I.Forward size={14} /> : <I.Copy size={14} />}
        </div>
        <div className="min-w-0 flex-1">
          <div className="truncate text-xs font-medium text-ink">{jobTitle(job.request, p)}</div>
          <div className="mt-0.5 truncate text-[11px] tabular-nums text-ink-muted">{detailLine(p)}</div>
        </div>
        <div className="flex shrink-0 items-center">
          {!done && p?.state !== "Scanning" && !p?.pending_conflict && (
            <button className="btn-icon size-6" aria-label={paused ? "Resume" : "Pause"} title={paused ? "Resume" : "Pause"} onClick={() => void controlOp(job.id, paused ? "Resume" : "Pause")}>
              {paused ? <I.Play size={12} /> : <I.Pause size={12} />}
            </button>
          )}
          {!done ? (
            <button className="btn-icon size-6 hover:text-danger!" aria-label="Cancel" title="Cancel" onClick={() => void controlOp(job.id, "Cancel")}>
              <I.Stop size={12} />
            </button>
          ) : (
            <button className="btn-icon size-6" aria-label="Dismiss" title="Dismiss" onClick={() => dismissJob(job.id)}>
              <I.Close size={12} />
            </button>
          )}
        </div>
      </div>

      <div className="mt-2.5 h-1.5 overflow-hidden rounded-full bg-base shadow-[inset_0_0_0_1px_var(--color-hairline)]">
        {frac === null ? (
          // Scanning: motion without a number.
          <div className="bar-indeterminate h-full w-full" />
        ) : (
          <div
            className={`h-full rounded-full transition-[width] duration-150 ease-linear ${
              failed ? "bg-danger" : paused ? "bg-warn" : p?.state === "Done" ? "bg-ok" : "bg-gradient-to-r from-accent-600 to-accent-400"
            }`}
            style={{ width: `${frac * 100}%` }}
          />
        )}
      </div>

      {!done && p?.current && <div className="mt-1.5 truncate text-[10px] text-ink-faint" title={p.current}>{p.current}</div>}
      {p && p.errors.length > 0 && (
        <details className="mt-2 text-[11px] text-ink-muted">
          <summary className="cursor-default text-danger">{p.errors.length} could not be processed</summary>
          <ul className="mt-1 max-h-24 space-y-0.5 overflow-y-auto">
            {p.errors.slice(0, 50).map((e, i) => (
              <li key={i} className="truncate" title={`${e.path}: ${e.message}`}>
                <span className="text-ink">{basename(e.path)}</span> — {e.message}
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}

function ConflictDialog({ op, conflict }: { op: OpId; conflict: Conflict }) {
  const [all, setAll] = useState(false);
  const firstRef = useRef<HTMLButtonElement>(null);
  useLayoutEffect(() => firstRef.current?.focus(), []);
  const resolve = (policy: ConflictPolicy) => void controlOp(op, { Resolve: { policy, apply_to_all: all } });
  useCommand("ui.cancel", () => resolve("Skip"));

  const name = basename(conflict.destination);
  const dot = name.lastIndexOf(".");
  const info = typeInfo({ kind: "File", ext: dot > 0 ? name.slice(dot + 1).toLowerCase() : null });
  const newer = conflict.source_modified > conflict.dest_modified ? "source" : conflict.source_modified < conflict.dest_modified ? "dest" : null;
  const larger = conflict.source_size > conflict.dest_size ? "source" : conflict.source_size < conflict.dest_size ? "dest" : null;

  return (
    <Modal label="File conflict" onClose={() => resolve("Skip")}>
      <div className="w-[520px] p-5">
        <h2 className="text-[14px] font-semibold text-ink">“{name}” already exists</h2>
        <p className="mt-1 text-xs text-ink-muted">
          The destination <span className="text-ink">{parent(conflict.destination)}</span> already has an item with this name.
        </p>

        <div className="mt-4 grid grid-cols-2 gap-2">
          <FileFacts label="Incoming" icon={info.icon} path={conflict.source} size={conflict.source_size} modified={conflict.source_modified} newer={newer === "source"} larger={larger === "source"} />
          <FileFacts label="Already there" icon={info.icon} path={conflict.destination} size={conflict.dest_size} modified={conflict.dest_modified} newer={newer === "dest"} larger={larger === "dest"} />
        </div>

        <div className="mt-4 grid grid-cols-2 gap-2">
          <button ref={firstRef} className="btn btn-primary justify-center" onClick={() => resolve("Overwrite")}>
            Replace
          </button>
          <button className="btn justify-center" onClick={() => resolve("Rename")}>
            Keep both
          </button>
          <button className="btn justify-center" onClick={() => resolve("KeepNewer")}>
            Keep the newer one
          </button>
          <button className="btn justify-center" onClick={() => resolve("Skip")}>
            Skip
          </button>
        </div>

        <div className="mt-4 flex items-center justify-between">
          <label className="flex cursor-default items-center gap-2 text-xs text-ink-muted">
            <input type="checkbox" checked={all} onChange={(e) => setAll(e.target.checked)} className="size-3.5 accent-[var(--color-accent-500)]" />
            Do this for all remaining conflicts
          </label>
          <button className="text-xs text-ink-faint hover:text-danger" onClick={() => void controlOp(op, "Cancel")}>
            Cancel operation
          </button>
        </div>
      </div>
    </Modal>
  );
}

function FileFacts(props: { label: string; icon: ReturnType<typeof typeInfo>["icon"]; path: string; size: number; modified: number; newer: boolean; larger: boolean }) {
  return (
    <div className="rounded-lg bg-base p-3 shadow-[inset_0_0_0_1px_var(--color-hairline)]">
      <div className="text-[10px] font-semibold uppercase tracking-wider text-ink-faint">{props.label}</div>
      <div className="mt-2 flex items-center gap-2">
        <FileIcon kind={props.icon} size={22} />
        <span className="truncate text-xs text-ink" title={props.path}>
          {basename(props.path)}
        </span>
      </div>
      <dl className="mt-2 space-y-1 text-[11px] tabular-nums">
        <div className="flex justify-between">
          <dt className="text-ink-faint">Size</dt>
          <dd className={props.larger ? "text-accent-200" : "text-ink-muted"}>
            {formatSize(props.size)} {props.larger && "· larger"}
          </dd>
        </div>
        <div className="flex justify-between">
          <dt className="text-ink-faint">Modified</dt>
          <dd className={props.newer ? "text-accent-200" : "text-ink-muted"}>
            {formatDate(props.modified)} {props.newer && "· newer"}
          </dd>
        </div>
      </dl>
    </div>
  );
}
