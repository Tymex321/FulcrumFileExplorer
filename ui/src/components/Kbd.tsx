/** Renders a command's current chord, live — so a rebind shows up everywhere at once. */
import { useSyncExternalStore } from "react";
import { chordParts } from "../lib/keymap";
import { keys } from "../state/commands";

export function Chord({ chord, className = "" }: { chord: string; className?: string }) {
  return (
    <span className={`inline-flex items-center gap-0.5 ${className}`}>
      {chordParts(chord).map((p, i) => (
        <kbd key={i} className="kbd">
          {p}
        </kbd>
      ))}
    </span>
  );
}

export function useChord(commandId: string): string | null {
  useSyncExternalStore(keys.subscribe, () => keys.getKeymap());
  return keys.chordsFor(commandId)[0] ?? null;
}

export function CommandChord({ id, className }: { id: string; className?: string }) {
  const chord = useChord(id);
  return chord ? <Chord chord={chord} className={className} /> : null;
}

/** Tooltip text for a button: "Back (Alt+←)". */
export function useTitle(label: string, commandId: string): string {
  const chord = useChord(commandId);
  return chord ? `${label} (${chordParts(chord).join("+")})` : label;
}
