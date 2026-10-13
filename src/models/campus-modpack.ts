export interface CampusModpackLine {
  id: number;
  gameVersion: string;
  loaderType: string;
  loaderVersion: string;
  versionNumber: string;
}

export interface CampusModpackProject {
  id: number;
  slug: string;
  name: string;
  summary: string;
  lines: CampusModpackLine[];
}

export interface CampusPublishOutcome {
  versionId: number;
  skipped: string[];
  /** What happened to the instance's own record of the modpack. */
  adoption: CampusPublishAdoption;
}

export type CampusPublishAdoption =
  | "followed"
  | "keptOther"
  | "notIsolated"
  | "localChanged"
  | "recordFailed";

export type CampusPublishStage =
  | "hashing"
  | "uploading"
  | "bundling"
  | "publishing";

export interface CampusPublishFileTransfer {
  name: string;
  done: number;
  total: number;
}

/** Byte-level detail while files are uploading. */
export interface CampusPublishTransfer {
  bytesDone: number;
  bytesTotal: number;
  /** Bytes per second. */
  speed: number;
  /** Files being sent at the same time. */
  concurrency: number;
  files: CampusPublishFileTransfer[];
}

export interface CampusPublishProgress {
  instanceId: string;
  stage: CampusPublishStage;
  done: number;
  total: number;
  transfer?: CampusPublishTransfer;
}

export interface CampusModpackRecommendation {
  slot: string;
  /** `campus`, or the platform of the project the slot points at. */
  platform: string;
  /** Set for a campus line. */
  project: CampusModpackProject | null;
  line: CampusModpackLine | null;
  /** Set for a project on another platform. */
  projectRef: string | null;
  /** Which line of that project to install. */
  gameVersion: string | null;
  loaderType: string | null;
}

export type CampusInstallStage = "fetching" | "applying" | "finalizing";

export interface CampusInstallProgress {
  instanceId: string;
  stage: CampusInstallStage;
  done: number;
  total: number;
}

export interface CampusInstallOutcome {
  versionId: string;
  loaderInstallPending: boolean;
}
