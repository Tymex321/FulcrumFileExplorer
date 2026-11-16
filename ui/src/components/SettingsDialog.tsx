/**
 * Settings, including the keybinding editor.
 *
 * Rebinding writes only overrides into `Settings::keybindings` (via `setBinding`), and
 * the dispatcher picks them up immediately — every tooltip, menu hint and palette chord
 * in the app updates live because they all read from the same registry.
 */
import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import type { Accent, Density, Settings, ViewMode } from "../api/types";
import { type CommandDef, findConflicts, normalizeChord, setBinding } from "../lib/keymap";
import { formatSize } from "../lib/format";
import { enableInstantSearch, openSettings, updateSettings, useApp } from "../state/app";
import { COMMANDS, keys, useCommand } from "../state/commands";
import { I } from "./Icons";
import { Chord } from "./Kbd";
import { Modal } from "./Overlays";

type Section = "general" | "keys" | "index";

export function SettingsDialog() {
  const open = useApp((s) => s.settingsOpen);
  const settings = useApp((s) => s.settings);
  const [section, setSection] = useState<Section>("general");
  useEffect(() => {
    if (open) setSection(open);
  }, [open]);
  useCommand("ui.cancel", () => openSettings(null), !!open);
  if (!open || !settings) return null;

  return (
    <Modal label="Settings" onClose={() => openSettings(null)} wide>
      <div className="relative flex h-[min(620px,84vh)]">
        <nav className="flex w-48 shrink-0 flex-col gap-0.5 border-r border-hairline bg-surface p-2">
          <div className="px-2.5 pb-2 pt-1 text-[13px] font-semibold text-ink">Settings</div>
          {(
            [
              ["general", "General", <I.Settings key="g" size={14} />],
              ["keys", "Keyboard shortcuts", <I.Keyboard key="k" size={14} />],
              ["index", "Instant search", <I.Bolt key="i" size={14} />],
            ] as const
          ).map(([id, label, icon]) => (
            <button
              key={id}
              onClick={() => setSection(id)}
              className={`flex h-8 items-center gap-2.5 rounded-md px-2.5 text-left text-xs transition-fast ${section === id ? "bg-raised text-ink" : "text-ink-muted hover:bg-raised/60 hover:text-ink"}`}
            >
              <span className={section === id ? "text-accent-300" : "text-ink-faint"}>{icon}</span>
              {label}
            </button>
          ))}
        </nav>
        <div className="min-w-0 flex-1 overflow-y-auto">
          {section === "general" && <General s={settings} />}
          {section === "keys" && <KeybindingEditor />}
          {section === "index" && <IndexSection />}
        </div>
        <button className="btn-icon absolute right-2 top-2" aria-label="Close settings" onClick={() => openSettings(null)}>
          <I.Close size={14} />
        </button>
      </div>
    </Modal>
  );
}

/* ───────────────────────────── General ───────────────────────────── */

const ACCENTS: { id: Accent; color: string }[] = [
  { id: "Purple", color: "#8b5cf6" },
  { id: "Violet", color: "#a855f7" },
  { id: "Magenta", color: "#d946ef" },
  { id: "Indigo", color: "#6366f1" },
];

function General({ s }: { s: Settings }) {
  const appearance = (patch: Partial<Settings["appearance"]>) => updateSettings((x) => ({ ...x, appearance: { ...x.appearance, ...patch } }));
  const behavior = (patch: Partial<Settings["behavior"]>) => updateSettings((x) => ({ ...x, behavior: { ...x.behavior, ...patch } }));
  const list = (patch: Partial<Settings["behavior"]["list"]>) => updateSettings((x) => ({ ...x, behavior: { ...x.behavior, list: { ...x.behavior.list, ...patch } } }));

  return (
    <div className="space-y-7 p-6">
      <Group title="Appearance">
        <Row label="Accent colour">
          <div className="flex gap-2">
            {ACCENTS.map((a) => (
              <button
                key={a.id}
                aria-label={a.id}
                aria-pressed={s.appearance.accent === a.id}
                title={a.id}
                onClick={() => appearance({ accent: a.id })}
                className={`size-6 rounded-full transition-fast ${s.appearance.accent === a.id ? "ring-2 ring-ink ring-offset-2 ring-offset-overlay" : "hover:scale-110"}`}
                style={{ background: a.color }}
              />
            ))}
          </div>
        </Row>
        <Row label="Row density" hint="How tall each row in the file list is.">
          <Seg<Density> value={s.appearance.density} options={["Compact", "Normal", "Comfortable"]} onChange={(density) => appearance({ density })} />
        </Row>
        <Row label="Text size">
          <Seg<string> value={String(s.appearance.font_size)} options={["12", "13", "14", "15"]} onChange={(v) => appearance({ font_size: Number(v) })} />
        </Row>
        <Toggle label="Animations" hint="Short transitions only; turning this off removes them entirely." value={s.appearance.animations} onChange={(animations) => appearance({ animations })} />
      </Group>

      <Group title="Browsing">
        <Row label="Default view for new tabs">
          <Seg<ViewMode> value={s.behavior.default_view} options={["Details", "List", "Tiles", "Grid"]} onChange={(default_view) => behavior({ default_view })} />
        </Row>
        <Toggle label="Show hidden files" value={s.behavior.list.show_hidden} onChange={(show_hidden) => list({ show_hidden })} />
        <Toggle label="Show protected system files" value={s.behavior.list.show_system} onChange={(show_system) => list({ show_system })} />
        <Toggle label="Open items with a single click" value={s.behavior.single_click_open} onChange={(single_click_open) => behavior({ single_click_open })} />
        <Toggle label="Restore tabs on launch" value={s.behavior.restore_session} onChange={(restore_session) => behavior({ restore_session })} />
      </Group>

      <Group title="Deleting">
        <Toggle label="Ask before deleting" value={s.behavior.confirm_delete} onChange={(confirm_delete) => behavior({ confirm_delete })} />
        <Toggle label="Delete permanently instead of using the Recycle Bin" hint="Off by default. Permanent deletes always ask first." value={s.behavior.skip_recycle_bin} onChange={(skip_recycle_bin) => behavior({ skip_recycle_bin })} />
      </Group>
    </div>
  );
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section>
      <h3 className="mb-2 text-[11px] font-semibold uppercase tracking-wider text-ink-faint">{title}</h3>
      <div className="divide-y divide-hairline rounded-lg bg-surface shadow-[inset_0_0_0_1px_var(--color-hairline)]">{children}</div>
    </section>
  );
}

function Row({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center gap-4 px-4 py-3">
      <div className="min-w-0 flex-1">
        <div className="text-xs text-ink">{label}</div>
        {hint && <div className="mt-0.5 text-[11px] text-ink-faint">{hint}</div>}
      </div>
      {children}
    </div>
  );
}

function Toggle({ label, hint, value, onChange }: { label: string; hint?: string; value: boolean; onChange: (v: boolean) => void }) {
  return (
    <Row label={label} hint={hint}>
      <button
        role="switch"
        aria-checked={value}
        aria-label={label}
        onClick={() => onChange(!value)}
        className={`relative h-5 w-9 shrink-0 rounded-full transition-fast ${value ? "bg-accent-600" : "bg-base shadow-[inset_0_0_0_1px_var(--color-hairline-strong)]"}`}
      >
        <span className={`absolute top-0.5 size-4 rounded-full bg-white shadow transition-fast ${value ? "left-[18px]" : "left-0.5 bg-ink-muted"}`} />
      </button>
    </Row>
  );
}

function Seg<T extends string>({ value, options, onChange }: { value: T; options: T[]; onChange: (v: T) => void }) {
  return (
    <div role="radiogroup" className="flex rounded-md bg-base p-0.5 shadow-[inset_0_0_0_1px_var(--color-hairline)]">
      {options.map((o) => (
        <button
          key={o}
          role="radio"
          aria-checked={value === o}
          onClick={() => onChange(o)}
          className={`h-6 rounded px-2.5 text-[11px] transition-fast ${value === o ? "bg-raised text-ink shadow-[inset_0_0_0_1px_var(--color-hairline-strong)]" : "text-ink-muted hover:text-ink"}`}
        >
          {o}
        </button>
      ))}
    </div>
  );
}

/* ───────────────────────────── Keyboard shortcuts ───────────────────────────── */

function KeybindingEditor() {
  const keymap = useSyncExternalStore(keys.subscribe, () => keys.getKeymap());
  const [filter, setFilter] = useState("");
  const [recording, setRecording] = useState<string | null>(null);
  const [showNav, setShowNav] = useState(false);
  const conflicts = useMemo(() => findConflicts(keymap), [keymap]);
  const overrides = useSyncExternalStore(keys.subscribe, () => keys.getOverrides());
  const overridden = new Set(overrides.map(([id]) => id));

  const apply = (cmd: CommandDef, chords: string[]) => {
    updateSettings((s) => ({ ...s, keybindings: setBinding(s.keybindings, cmd, chords) }));
  };

  const record = (cmd: CommandDef) => {
    setRecording(cmd.id);
    keys.record((chord) => {
      setRecording(null);
      if (!chord) return; // Escape cancels
      const current = keys.chordsFor(cmd.id);
      if (!current.includes(chord)) apply(cmd, [...current, chord]);
    });
  };
  useEffect(() => () => keys.record(null), []);

  // Searching by chord works too: type "ctrl+t" to find what owns it.
  const q = filter.trim().toLowerCase();
  const chordQuery = normalizeChord(filter);
  const visible = COMMANDS.filter((c) => {
    if (c.hidden && !showNav && !q) return false;
    if (!q) return true;
    const chords = keymap.byCommand.get(c.id) ?? [];
    return c.title.toLowerCase().includes(q) || c.category.toLowerCase().includes(q) || c.id.includes(q) || (chordQuery !== null && chords.includes(chordQuery));
  });
  const categories = [...new Set(visible.map((c) => c.category))];

  return (
    <div className="p-6">
      <div className="flex items-center gap-3">
        <label className="field flex flex-1 items-center gap-2">
          <I.Search size={13} className="text-ink-faint" />
          <input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Search commands or chords (e.g. ctrl+t)" className="h-full flex-1 bg-transparent text-xs outline-none placeholder:text-ink-faint" />
        </label>
        <label className="flex shrink-0 items-center gap-2 text-[11px] text-ink-muted">
          <input type="checkbox" checked={showNav} onChange={(e) => setShowNav(e.target.checked)} className="accent-[var(--color-accent-500)]" />
          Show list navigation
        </label>
        {overrides.length > 0 && (
          <button className="btn shrink-0" onClick={() => updateSettings((s) => ({ ...s, keybindings: [] }))}>
            Reset all
          </button>
        )}
      </div>
      {conflicts.size > 0 && (
        <div className="mt-3 flex items-center gap-2 rounded-md bg-[color-mix(in_oklab,var(--color-warn)_9%,transparent)] px-3 py-2 text-[11px] text-ink-muted">
          <I.Warn size={13} className="text-warn" />
          {conflicts.size} chord{conflicts.size === 1 ? " is" : "s are"} bound to more than one command. The most recently opened view wins, which is rarely what you want.
        </div>
      )}

      {categories.map((cat) => (
        <section key={cat} className="mt-5">
          <h3 className="mb-1.5 text-[11px] font-semibold uppercase tracking-wider text-ink-faint">{cat}</h3>
          <div className="divide-y divide-hairline rounded-lg bg-surface shadow-[inset_0_0_0_1px_var(--color-hairline)]">
            {visible
              .filter((c) => c.category === cat)
              .map((c) => {
                const chords = keymap.byCommand.get(c.id) ?? [];
                return (
                  <div key={c.id} className="group flex min-h-10 items-center gap-3 px-4 py-1.5">
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-xs text-ink">{c.title}</div>
                      <div className="truncate font-mono text-[10px] text-ink-faint">{c.id}</div>
                    </div>
                    <div className="flex flex-wrap items-center justify-end gap-1.5">
                      {chords.map((ch) => {
                        const clash = conflicts.get(ch)?.filter((id) => id !== c.id);
                        return (
                          <span
                            key={ch}
                            title={clash?.length ? `Also bound to: ${clash.map((id) => keys.command(id)?.title ?? id).join(", ")}` : undefined}
                            className={`flex items-center gap-1 rounded-md py-0.5 pl-1 pr-0.5 ${clash?.length ? "bg-[color-mix(in_oklab,var(--color-warn)_14%,transparent)]" : ""}`}
                          >
                            <Chord chord={ch} />
                            <button aria-label={`Remove ${ch}`} className="grid size-4 place-items-center rounded text-ink-faint opacity-0 transition-fast hover:text-danger group-hover:opacity-100" onClick={() => apply(c, chords.filter((x) => x !== ch))}>
                              <I.Close size={10} />
                            </button>
                          </span>
                        );
                      })}
                      {recording === c.id ? (
                        <span className="rounded-md bg-select px-2 py-0.5 text-[11px] text-accent-100 shadow-[inset_0_0_0_1px_var(--color-accent-600)]" style={{ animation: "fade-in var(--dur) var(--ease-out)" }}>
                          Press a shortcut… <span className="text-ink-faint">(Esc to cancel)</span>
                        </span>
                      ) : (
                        <button className="btn h-6 px-2 text-[11px] opacity-0 group-hover:opacity-100 focus-visible:opacity-100" onClick={() => record(c)}>
                          <I.Plus size={11} /> Add
                        </button>
                      )}
                      {overridden.has(c.id) && (
                        <button className="h-6 rounded px-1.5 text-[11px] text-accent-300 hover:bg-raised" title="Restore default" onClick={() => apply(c, c.keys)}>
                          Reset
                        </button>
                      )}
                    </div>
                  </div>
                );
              })}
          </div>
        </section>
      ))}
    </div>
  );
}

/* ───────────────────────────── Index ───────────────────────────── */

function IndexSection() {
  const index = useApp((s) => s.index);
  const enabling = useApp((s) => s.enabling);
  const volumes = useApp((s) => s.volumes);
  const ready = index?.state === "Ready";
  return (
    <div className="space-y-7 p-6">
      <div className="rounded-xl bg-gradient-to-br from-[color-mix(in_oklab,var(--color-accent-700)_25%,transparent)] to-transparent p-5 shadow-[inset_0_0_0_1px_color-mix(in_oklab,var(--color-accent-500)_30%,transparent)]">
        <div className="flex items-center gap-2 text-sm font-semibold text-ink">
          <I.Bolt size={16} className="text-accent-300" />
          {ready ? "Instant search is on" : index?.state === "Scanning" ? "Building the index…" : "Instant search is off"}
        </div>
        <p className="mt-2 max-w-lg text-xs leading-relaxed text-ink-muted">
          Fulcrum reads the NTFS Master File Table directly and follows the USN change journal, so it can search a whole drive in microseconds instead of walking every folder. Reading the MFT needs
          administrator rights, so that part runs in a small, read-only background service installed once. The Fulcrum window itself never runs elevated.
        </p>
        {!ready && (
          <button className="btn btn-primary mt-4" disabled={enabling || index?.state === "Scanning"} onClick={() => void enableInstantSearch()}>
            {enabling ? "Waiting for permission…" : index?.state === "Scanning" ? "Indexing…" : "Enable instant search"}
          </button>
        )}
      </div>
      {index && (
        <Group title="Index">
          <Row label="State">
            <span className="text-xs text-ink-muted">{index.state}</span>
          </Row>
          <Row label="Files and folders">
            <span className="text-xs tabular-nums text-ink-muted">
              {index.total_entries.toLocaleString()} ({index.total_dirs.toLocaleString()} folders)
            </span>
          </Row>
          <Row label="Memory used" hint="Kept compact: under 40 bytes per entry.">
            <span className="text-xs tabular-nums text-ink-muted">{formatSize(index.memory_bytes)}</span>
          </Row>
          <Row label="Last full scan">
            <span className="text-xs tabular-nums text-ink-muted">{index.last_scan_millis ? `${(index.last_scan_millis / 1000).toFixed(1)} s` : "—"}</span>
          </Row>
          <Row label="Volumes">
            <span className="text-xs text-ink-muted">
              {volumes.map((v) => `${v.id}: ${v.indexable ? (index.volumes.includes(v.id) ? "indexed" : "not indexed") : "not indexable"}`).join(" · ")}
            </span>
          </Row>
        </Group>
      )}
    </div>
  );
}
