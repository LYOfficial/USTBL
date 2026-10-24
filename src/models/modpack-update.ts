import { ModLoaderType } from "@/enums/instance";

export type ModpackCheckStatus =
  | "notFollowing"
  | "paused"
  | "needsRepair"
  | "waitingLoader"
  | "upToDate"
  | "localChanges"
  | "available"
  | "unknown";

export interface ModpackUpdateCheck {
  status: ModpackCheckStatus;
  follow: boolean;
  currentLabel?: string;
  latestVersionId?: string;
  latestLabel?: string;
  /** Error code explaining `unknown` or `needsRepair`. */
  reason?: string;
  /** The previous update left a backup that can still be rolled back. */
  canRollBack: boolean;
  /** Managed files that differ from the install; set by the local scan. */
  localChanges?: number;
  /** The platform confirmed the project is gone; this check detached the instance. */
  unlinked?: boolean;
}

export interface ModpackFileKey {
  sha1: string;
  size: number;
}

export type ModpackChangeKind =
  | "add"
  | "update"
  | "delete"
  /** The file only moves between its enabled and disabled variants. */
  | "disable"
  | "enable"
  /** The modpack stops managing the file but leaves it on disk. */
  | "retire";

export interface ModpackFileChange {
  path: string;
  kind: ModpackChangeKind;
  remote?: ModpackFileKey;
  localSize?: number;
}

export type ModpackConflictKind =
  | "modifiedLocallyChangedRemotely"
  | "modifiedLocallyRemovedRemotely"
  | "deletedLocallyChangedRemotely"
  | "unmanagedFileInTheWay"
  /** A file the modpack left alone is managed again with other content. */
  | "remanage";

export interface ModpackConflict {
  path: string;
  kind: ModpackConflictKind;
  baseline?: ModpackFileKey;
  local?: ModpackFileKey;
  remote?: ModpackFileKey;
  hunks: ModpackMergeConflict[];
  /** The player ignored the local change here: the file, or every block. */
  ignored?: boolean;
  /** Blocks (indexes into `hunks`) whose local changes are all ignored. */
  ignoredHunks?: number[];
}

export interface ModpackMergeConflict {
  baseStart: number;
  baseEnd: number;
  /** `base[baseStart..baseEnd]`, the lines both sides changed. */
  base: string[];
  /** The same range as each side has it. */
  local: string[];
  remote: string[];
}

export type ModpackDiffLineKind = "context" | "add" | "delete";

export interface ModpackDiffLine {
  kind: ModpackDiffLineKind;
  /** 1-based; absent on added lines. */
  oldNumber?: number | null;
  /** 1-based; absent on deleted lines. */
  newNumber?: number | null;
  text: string;
  /** Which of the hunk's `edits` an added or deleted line belongs to. */
  edit?: number | null;
}

/** The old-side line range of one change, 0-based, `[start, end)`. */
export interface ModpackDiffEdit {
  start: number;
  end: number;
}

/** One `@@ -oldStart,oldLines +newStart,newLines @@` block. */
export interface ModpackDiffHunk {
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  lines: ModpackDiffLine[];
  /** The changes this hunk shows, in order; only local-change diffs have them. */
  edits?: ModpackDiffEdit[];
}

export type ModpackLocalChangeKind =
  | "modified"
  | "deleted"
  /** Only the disabled state differs: the file sits at the other variant. */
  | "disabled"
  | "enabled";

export interface ModpackLocalChange {
  path: string;
  kind: ModpackLocalChangeKind;
  baseline: ModpackFileKey;
  local?: ModpackFileKey | null;
  /** Normalized line diff, installed → local; only for text files. */
  hunks?: ModpackDiffHunk[] | null;
  noDiff?: "notText" | "baseMissing" | "bothVariants" | null;
  /** A modified file whose disabled state also flipped. */
  toggled?: boolean;
}

export type ModpackConflictChoice = "keepLocal" | "takeRemote";

/** A line range of the installed text the player keeps as their own. */
export interface ModpackIgnoredRegion {
  start: number;
  end: number;
  baseLines: string[];
  localLines: string[];
}

export interface ModpackIgnoredItem {
  path: string;
  baseSha1: string;
  /** The whole file is ignored. */
  whole: boolean;
  regions: ModpackIgnoredRegion[];
}

export interface ModpackBaselineLoader {
  type: ModLoaderType;
  version: string;
}

export interface ModpackUpdatePlan {
  fromVersionId: string;
  toVersionId: string;
  toLabel: string;
  changes: ModpackFileChange[];
  conflicts: ModpackConflict[];
  autoMerged: ModpackFileChange[];
  loaderChange?: { from: ModpackBaselineLoader; to: ModpackBaselineLoader };
  reinstallRequired: boolean;
  fastForward: boolean;
  planHash: string;
}

export interface ModpackUpdateOutcome {
  versionId: string;
  loaderInstallPending: boolean;
}
