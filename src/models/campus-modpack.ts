export interface CampusModpackLine {
  id: number;
  gameVersion: string;
  loaderType: string;
  loaderVersion: string;
  versionNumber: string;
  /** Changelog of the newest version; absent from older platforms. */
  changelog?: string | null;
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

export interface CampusPublishFile {
  /** Always the enabled name. */
  path: string;
  disabled: boolean;
  once: boolean;
}

export interface CampusPublishRule {
  path: string;
  kind: "file" | "dir";
  state: "ignore" | "delete";
}

/**
 * The publish tree's starting state, worked out by the launcher from the
 * files on disk and the line's current release.
 */
export interface CampusPublishDefaults {
  /** Enabled names; `conflict` when both variants are on disk. */
  files: { path: string; disabled: boolean; conflict: boolean }[];
  /** Paths the current release mentions that are not on disk any more. */
  ghosts: { path: string; kind: "file" | "dir" }[];
  rules: CampusPublishRule[];
  /** Lowercased paths of the files ticked explicitly. */
  included: string[];
  /** Lowercased paths of the files that install only once. */
  once: string[];
}

export interface CampusNewProject {
  name: string;
  summary: string;
}

/** A local publish record that can survive launcher restarts. */
export interface CampusUnfinishedPublish {
  jobId: string;
  instanceId: string;
  instanceName: string;
  projectSlug: string;
  versionNumber: string;
  phase: "uploading" | "processing" | "failed";
  error: string | null;
  /** Whether "继续" may be offered; a terminal failure only gets "放弃". */
  resumable: boolean;
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

export type CampusInstallStage =
  | "waitingClient"
  | "preparing"
  | "locating"
  | "fetching"
  | "applying"
  | "finalizing";

export interface CampusInstallProgress {
  instanceId: string;
  group?: string | null;
  stage: CampusInstallStage;
  done: number;
  total: number;
  bytesDone: number;
  bytesTotal: number;
}

export interface CampusInstallStarted {
  group: string;
}

export interface CampusInstallFinished {
  instanceId: string;
  group: string;
  error: string | null;
}

export interface CampusUnfinishedInstall {
  instanceId: string;
  instanceName: string;
  label: string;
  phase: "waitingClient" | "installing" | "failed";
  error: string | null;
  resumable: boolean;
  group: string | null;
  progress: Omit<CampusInstallProgress, "instanceId"> | null;
}

export interface CampusInstallOutcome {
  versionId: string;
  loaderInstallPending: boolean;
}
