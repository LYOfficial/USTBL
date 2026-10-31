import type { TFunction } from "i18next";
import type {
  CampusInstallProgress,
  CampusUnfinishedInstall,
} from "@/models/campus-modpack";
import type {
  GTaskEventStatusEnums,
  TaskDesc,
  TaskGroupDesc,
} from "@/models/task";
import { localizeFileError } from "@/utils/file-error";

export type CampusInstallCardState =
  | "active-install"
  | "paused"
  | "completed"
  | "failed"
  | "cancelled";

/** Keep the task-card state mapping independent from React rendering. */
export const campusInstallCardState = (
  status: GTaskEventStatusEnums | string,
  reason?: string
): CampusInstallCardState => {
  if (status === "Completed") return "completed";
  if (status === "Failed" || reason) return "failed";
  if (status === "Cancelled") return "cancelled";
  if (status === "Stopped") return "paused";
  return "active-install";
};

export const campusInstallStageKey = (filename: string): string =>
  filename.trim().toLowerCase() === "finalizing"
    ? "applying"
    : filename.trim().toLowerCase();

export const campusInstallProgressView = (
  progress?: Omit<CampusInstallProgress, "instanceId"> | null
) =>
  progress
    ? {
        stage: campusInstallStageKey(progress.stage),
        done: progress.done,
        total: progress.total,
        bytes:
          progress.stage === "fetching"
            ? { done: progress.bytesDone, total: progress.bytesTotal }
            : null,
      }
    : null;

// The backend only lists idle installs. Always show their actions at the top,
// even when the failed task group is still visible below.
export const visibleCampusInstalls = (jobs: CampusUnfinishedInstall[]) => jobs;

export const removeCampusInstallGroups = (
  tasks: TaskGroupDesc[],
  groups: string[]
) => tasks.filter((task) => !groups.includes(task.taskGroup));

const campusInstallGroupName = (group: string): string => {
  const end = group.lastIndexOf("@");
  return group.slice(group.indexOf("?") + 1, end < 0 ? undefined : end);
};

// The backend writes the instance id into the group name with these
// characters replaced (campus_install_group_belongs_to).
const groupIdentitySuffix = (instanceId: string): string =>
  ` [${instanceId.replace(/[?@\n\r]/g, " ")}]`;

// The longest known id whose " [<id>]" ends the group name. Instance ids may
// contain brackets themselves, so splitting on the last " [" is only the
// fallback for ids that are not known (for example a deleted instance).
const matchKnownIdentity = (
  name: string,
  knownIds: readonly string[]
): { id: string; start: number } | null => {
  let best: { id: string; start: number } | null = null;
  for (const id of knownIds) {
    const suffix = groupIdentitySuffix(id);
    if (
      name.endsWith(suffix) &&
      (best === null || suffix.length > name.length - best.start)
    ) {
      best = { id, start: name.length - suffix.length };
    }
  }
  return best;
};

export const campusInstallGroupLabel = (
  group: string,
  knownIds: readonly string[] = []
): string => {
  const name = campusInstallGroupName(group);
  const known = matchKnownIdentity(name, knownIds);
  if (known) return name.slice(0, known.start);
  const identity = name.lastIndexOf(" [");
  return identity < 0 ? name : name.slice(0, identity);
};

// "modpack-install?<label> [<instance id>]@<time>": the instance id as the
// backend wrote it into the group name.
export const campusInstallGroupInstanceId = (
  group: string,
  knownIds: readonly string[] = []
): string | null => {
  const name = campusInstallGroupName(group);
  const known = matchKnownIdentity(name, knownIds);
  if (known) return known.id;
  const start = name.lastIndexOf(" [");
  return start < 0 || !name.endsWith("]")
    ? null
    : name.slice(start + 2, name.length - 1);
};

const pastCommitStages = ["applying", "finalizing"];

/**
 * Whether a running first install can still be abandoned. The backend stops
 * accepting the request once it starts writing into the instance (its
 * "applying" stage), so the button follows the same stage and stays the
 * backend's decision in the end.
 */
export const canDiscardRunningInstall = (
  instanceId: string,
  tasks: TaskGroupDesc[],
  progress: Record<string, Pick<CampusInstallProgress, "instanceId" | "stage">>,
  knownIds: readonly string[] = []
): boolean => {
  const ids = [instanceId, ...knownIds];
  const running = tasks.filter(
    (group) =>
      isCampusInstallGroup(group.taskGroup) &&
      String(group.status) === "Started" &&
      (progress[group.taskGroup]?.instanceId ??
        campusInstallGroupInstanceId(group.taskGroup, ids)) === instanceId
  );
  return !running.some(
    (group) =>
      pastCommitStages.includes(progress[group.taskGroup]?.stage ?? "") ||
      group.taskDescs.some(
        (task) =>
          campusInstallStageKey(task.payload.filename) === "applying" &&
          String(task.status) !== "Waiting"
      )
  );
};

const installStages = [
  "waitingclient",
  "preparing",
  "locating",
  "fetching",
  "applying",
];

export const compareCampusInstallStages = (a: TaskDesc, b: TaskDesc): number =>
  installStages.indexOf(campusInstallStageKey(a.payload.filename)) -
  installStages.indexOf(campusInstallStageKey(b.payload.filename));

export const campusInstallTaskIndeterminate = (task: TaskDesc): boolean =>
  task.status === "InProgress" && !task.total;

export const isCampusInstallGroup = (group: string): boolean =>
  group.startsWith("modpack-install?");

export const localizeCampusInstallError = (
  t: TFunction,
  reason?: string
): string => {
  if (!reason) return t("DownloadTasksPage.label.error");
  const fileMessage = localizeFileError(t, reason);
  if (fileMessage) return fileMessage;
  if (
    /network|connection|timeout|timed out|request|http|status|range ended/i.test(
      reason
    )
  ) {
    return t(
      "Services.instance.installCampusModpack.error.description.NETWORK_ERROR"
    );
  }
  if (
    /os error|permission|access.*denied|disk|space|file creation|read.only/i.test(
      reason
    )
  ) {
    return t(
      "Services.instance.installCampusModpack.error.description.FILE_OPERATION_ERROR"
    );
  }
  const key = `Services.instance.installCampusModpack.error.description.${reason}`;
  const fallback = t(`UpdateModpackModal.reason.${reason}`, {
    defaultValue: t(
      "Services.instance.installCampusModpack.error.description.UNKNOWN_ERROR"
    ),
  });
  return t(key, { defaultValue: fallback });
};
