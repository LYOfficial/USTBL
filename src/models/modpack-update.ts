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
  loaderChange?: { from: ModpackBaselineLoader; to: ModpackBaselineLoader };
  reinstallRequired: boolean;
  fastForward: boolean;
  planHash: string;
}

export interface ModpackUpdateOutcome {
  versionId: string;
  loaderInstallPending: boolean;
}
