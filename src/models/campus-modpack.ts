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
}

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
