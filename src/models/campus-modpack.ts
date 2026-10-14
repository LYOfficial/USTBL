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
export type CampusPublishStage = "hashing";

export interface CampusPublishProgress {
  instanceId: string;
  stage: CampusPublishStage;
  done: number;
  total: number;
}

/** The upload went to the background; its task group is on the task list. */
export interface CampusPublishStarted {
  group: string;
}

export interface CampusPublishFinished {
  instanceId: string;
  group: string;
  outcome?: CampusPublishOutcome;
  /** Error code or message when the publish failed. */
  error?: string;
}
