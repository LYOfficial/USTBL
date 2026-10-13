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

export interface CampusPublishProgress {
  instanceId: string;
  stage: CampusPublishStage;
  done: number;
  total: number;
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
