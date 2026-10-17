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

/** What happens before the upload, while the publish dialog is still open. */
export type CampusPublishStage = "hashing" | "packing";

export interface CampusPublishProgress {
  instanceId: string;
  stage: CampusPublishStage;
  done: number;
  total: number;
}

/** The upload went to the background; its task group is on the task list. */
export interface CampusPublishStarted {
  group: string;
  jobId: string;
}

export interface CampusNewProject {
  name: string;
  summary: string;
}

/** A publish that was interrupted or failed and can be continued or given up. */
export interface CampusUnfinishedPublish {
  jobId: string;
  instanceId: string;
  instanceName: string;
  projectSlug: string;
  versionNumber: string;
  phase: "uploading" | "processing" | "failed";
  error: string | null;
  /** On the task list right now. */
  running: boolean;
  createdAt: number;
}

export interface CampusPublishFinished {
  instanceId: string;
  group: string;
  jobId: string;
  outcome?: CampusPublishOutcome;
  /** Error code or message when the publish failed. */
  error?: string;
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
