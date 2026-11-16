/**
 * Re-exports the Rust-generated types.
 *
 * `ui/src/bindings/` is produced by ts-rs from the `#[derive(TS)]` types in `fx-core`
 * (`scripts/gen-bindings.sh`). Import backend types from here, never by hand-writing an
 * interface — the whole point is that a Rust-side change becomes a TypeScript compile
 * error instead of a runtime surprise.
 */
export type { FileEntry } from "../bindings/FileEntry";
export type { EntryKind } from "../bindings/EntryKind";
export type { Attributes } from "../bindings/Attributes";
export type { VolumeInfo } from "../bindings/VolumeInfo";
export type { Filesystem } from "../bindings/Filesystem";

export type { Query } from "../bindings/Query";
export type { MatchMode } from "../bindings/MatchMode";
export type { CaseMode } from "../bindings/CaseMode";
export type { SearchScope } from "../bindings/SearchScope";
export type { Filters } from "../bindings/Filters";
export type { Sort } from "../bindings/Sort";
export type { SortKey } from "../bindings/SortKey";
export type { SearchHit } from "../bindings/SearchHit";
export type { QuerySummary } from "../bindings/QuerySummary";
export type { SizeRange } from "../bindings/SizeRange";
export type { TimeRange } from "../bindings/TimeRange";

export type { ListOptions } from "../bindings/ListOptions";
export type { ListSummary } from "../bindings/ListSummary";
export type { Progress } from "../bindings/Progress";
export type { OpId } from "../bindings/OpId";
export type { OpState } from "../bindings/OpState";
export type { OpControl } from "../bindings/OpControl";
export type { FsOpKind } from "../bindings/FsOpKind";
export type { FsOpRequest } from "../bindings/FsOpRequest";
export type { ConflictPolicy } from "../bindings/ConflictPolicy";
export type { Conflict } from "../bindings/Conflict";
export type { Thumbnail } from "../bindings/Thumbnail";

export type { IndexStats } from "../bindings/IndexStats";
export type { IndexState } from "../bindings/IndexState";
export type { ChangeEvent } from "../bindings/ChangeEvent";

export type { Settings } from "../bindings/Settings";
export type { Appearance } from "../bindings/Appearance";
export type { Accent } from "../bindings/Accent";
export type { Density } from "../bindings/Density";
export type { ViewMode } from "../bindings/ViewMode";
export type { PaneLayout } from "../bindings/PaneLayout";
export type { SessionState } from "../bindings/SessionState";
export type { TabState } from "../bindings/TabState";
export type { FxError } from "../bindings/FxError";
