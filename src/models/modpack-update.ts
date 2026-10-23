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

export type ModpackChangeKind = "add" | "update" | "delete";

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
  | "disabledLocally";

export interface ModpackConflict {
  path: string;
  kind: ModpackConflictKind;
  baseline?: ModpackFileKey;
  local?: ModpackFileKey;
  remote?: ModpackFileKey;
  hunks: ModpackMergeConflict[];
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
}

/** One `@@ -oldStart,oldLines +newStart,newLines @@` block. */
export interface ModpackDiffHunk {
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  lines: ModpackDiffLine[];
}

export type ModpackLocalChangeKind = "modified" | "deleted" | "disabled";

export interface ModpackLocalChange {
  path: string;
  kind: ModpackLocalChangeKind;
  baseline: ModpackFileKey;
  local?: ModpackFileKey | null;
  /** Normalized line diff, installed → local; only for text files. */
  hunks?: ModpackDiffHunk[] | null;
  noDiff?: "notText" | "baseMissing" | null;
}

export type ModpackConflictChoice = "keepLocal" | "takeRemote";

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
