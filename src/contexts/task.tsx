import { useToast as useChakraToast } from "@chakra-ui/react";
import { emit } from "@tauri-apps/api/event";
import { useRouter } from "next/router";
import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useSharedModals } from "@/contexts/shared-modal";
import { useToast } from "@/contexts/toast";
import { OtherResourceType } from "@/enums/resource";
import {
  CampusInstallProgress,
  CampusUnfinishedInstall,
  CampusUnfinishedPublish,
} from "@/models/campus-modpack";
import { ModpackUpdateFinished } from "@/models/modpack-update";
import {
  CreatedPTaskEventStatus,
  FailedPTaskEventStatus,
  GTaskEventPayload,
  GTaskEventStatusEnums,
  InProgressPTaskEventStatus,
  PTaskEventPayload,
  PTaskEventStatusEnums,
  StartedPTaskEventStatus,
  TaskDesc,
  TaskDescStatusEnums,
  TaskGroupDesc,
  TaskParam,
  TaskPayload,
  TaskTypeEnums,
} from "@/models/task";
import { CampusModpackService } from "@/services/campus-modpack";
import { ConfigService } from "@/services/config";
import { InstanceService } from "@/services/instance";
import { ModpackUpdateService } from "@/services/modpack-update";
import { TaskService } from "@/services/task";
import {
  compareCampusInstallStages,
  isCampusInstallGroup,
  localizeCampusInstallError,
  removeCampusInstallGroups,
} from "@/utils/campus-install-task";
import { takeLoaderCompletions } from "@/utils/loader-completion";
import {
  finishModpackUpdate,
  isModpackUpdateGroup,
  localizeModpackUpdateError,
} from "@/utils/modpack-update-task";

interface TaskContextType {
  installProgress: Record<string, CampusInstallProgress>;
  updateResults: Record<string, ModpackUpdateFinished>;
  tasks: TaskGroupDesc[];
  generalPercent: number | undefined; // General progress percentage for all tasks
  /** Publish records waiting on the user (§3.2.3); the nav-bar task button
   * counts these too, so it stays visible after a restart. */
  unfinishedPublishes: CampusUnfinishedPublish[];
  unfinishedInstalls: CampusUnfinishedInstall[];
  refreshUnfinishedInstalls: () => void;
  refreshUnfinishedPublishes: () => void;
  handleScheduleProgressiveTaskGroup: (
    taskGroup: string,
    params: TaskParam[]
  ) => void;
  handleCancelProgressiveTaskGroup: (taskGroup: string) => void;
  handleStopProgressiveTaskGroup: (taskGroup: string) => void;
  handleResumeProgressiveTaskGroup: (taskGroup: string) => void;
  handleClearHistoryTaskGroups: () => void;
}

export const TaskContext = createContext<TaskContextType | undefined>(
  undefined
);

export const TaskContextProvider: React.FC<{ children: React.ReactNode }> = ({
  children,
}) => {
  const toast = useToast();
  const { close: closeToast } = useChakraToast();
  const { getInstanceList, checkModpackUpdate } = useGlobalData();
  const router = useRouter();
  const { getJavaInfos } = useLauncherConfig();
  const { openSharedModal } = useSharedModals();
  const [tasks, setTasks] = useState<TaskGroupDesc[]>([]);
  const [installProgress, setInstallProgress] = useState<
    Record<string, CampusInstallProgress>
  >({});
  const [unfinishedInstalls, setUnfinishedInstalls] = useState<
    CampusUnfinishedInstall[]
  >([]);
  const [generalPercent, setGeneralPercent] = useState<number>();
  const [updateResults, setUpdateResults] = useState<
    Record<string, ModpackUpdateFinished>
  >({});
  const { t } = useTranslation();
  const loaderCompletionClaims = React.useRef(new Map<string, Set<string>>());
  const loaderInstallsInFlight = React.useRef(new Set<string>());
  const loaderPreparationsClaimed = React.useRef(new Set<string>());
  const [loaderCompletionTick, setLoaderCompletionTick] = useState(0);

  useEffect(() => {
    for (const instanceId of takeLoaderCompletions(
      tasks,
      loaderCompletionClaims.current,
      loaderInstallsInFlight.current
    )) {
      loaderInstallsInFlight.current.add(instanceId);
      const instanceName = getInstanceList()?.find(
        (i) => i.id === instanceId
      )?.name;
      const loadingToast = toast({
        title: t("Services.instance.finishModLoaderInstall.loading", {
          instanceName,
        }),
        status: "loading",
      });
      InstanceService.finishModLoaderInstall(instanceId)
        .then((response) => {
          if (response.status === "success" && !response.data) return;
          toast({
            title: response.message,
            description:
              response.status === "success" ? undefined : response.details,
            status: response.status === "success" ? "success" : "error",
          });
          if (response.status === "success") getInstanceList(true);
        })
        .catch((error) =>
          logger.error("Failed to finish loader install:", error)
        )
        .finally(() => {
          closeToast(loadingToast);
          loaderInstallsInFlight.current.delete(instanceId);
          // A second runtime group may have finished while this invocation
          // waited for the backend's scan to publish the instance.
          setLoaderCompletionTick((tick) => tick + 1);
        });
    }
  }, [tasks, loaderCompletionTick, getInstanceList, toast, t, closeToast]);

  const updateGroupInfo = useCallback((group: TaskGroupDesc) => {
    if (group.status === GTaskEventStatusEnums.Completed) {
      group.taskDescs.forEach((t) => {
        t.status = TaskDescStatusEnums.Completed;
        t.current = t.total; // Ensure current is set to total for completed tasks
      });
    }

    group.finishedCount = group.taskDescs.filter(
      (t) => t.status === TaskDescStatusEnums.Completed
    ).length;

    let knownTotalArr = group.taskDescs.filter((t) => t.total && t.total > 0);
    let knownTotal = knownTotalArr.reduce((acc, t) => acc + t.total, 0);
    let knownCurrent = knownTotalArr.reduce(
      (acc, t) => acc + (t.current || 0),
      0
    );
    let estimatedTotal;
    if (knownTotalArr.length > 0) {
      estimatedTotal =
        knownTotal +
        (group.taskDescs.length - knownTotalArr.length) *
          (knownTotal / knownTotalArr.length); // Estimate unknown task's size based on known tasks' average size
    } else {
      estimatedTotal = knownTotal; // Fallback when no known tasks exist
    }

    group.progress = estimatedTotal ? (knownCurrent * 100) / estimatedTotal : 0;

    group.estimatedTime = undefined;
    group.taskDescs.forEach((t) => {
      if (t.status === TaskDescStatusEnums.InProgress && t.estimatedTime) {
        if (
          !group.estimatedTime ||
          group.estimatedTime.secs < t.estimatedTime.secs
        ) {
          group.estimatedTime = t.estimatedTime;
        }
      }
      t.progress = t.total ? (t.current * 100) / t.total : 0;
    });
    group.taskDescs.sort((a, b) => {
      if (
        isCampusInstallGroup(group.taskGroup) ||
        isModpackUpdateGroup(group.taskGroup)
      ) {
        return compareCampusInstallStages(a, b);
      }
      let level = (desc: TaskDesc) => {
        switch (desc.status) {
          case TaskDescStatusEnums.Failed:
            return 0;
          case TaskDescStatusEnums.InProgress:
            return 1;
          case TaskDescStatusEnums.Waiting:
            return 2;
          case TaskDescStatusEnums.Completed:
            return 4;
          default:
            return 3;
        }
      };
      return level(a) - level(b);
    });
  }, []);

  const handleRetrieveProgressTasks = useCallback(() => {
    TaskService.retrieveProgressiveTaskList().then((response) => {
      if (response.status === "success") {
        logger.info("Retrieved progressive tasks:", response.data);
        // info(JSON.stringify(response.data));
        setTasks((prevTasks) => {
          let tasks = response.data
            .map((group) => {
              let prevGroup = prevTasks?.find(
                (t) => t.taskGroup === group.taskGroup
              );
              if (prevGroup) return prevGroup;
              updateGroupInfo(group);
              return group;
            })
            .filter(
              (group) => group.status !== GTaskEventStatusEnums.Cancelled
            );
          tasks.sort((a, b) => {
            let { timestamp: aTime } = parseTaskGroup(a.taskGroup);
            let { timestamp: bTime } = parseTaskGroup(b.taskGroup);
            return bTime - aTime; // Sort by timestamp descending
          });
          return tasks;
        });
      } else {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
    });
  }, [toast, updateGroupInfo]);

  useEffect(() => {
    handleRetrieveProgressTasks();
  }, [handleRetrieveProgressTasks]);

  const handleScheduleProgressiveTaskGroup = useCallback(
    (taskGroup: string, params: TaskParam[]) => {
      TaskService.scheduleProgressiveTaskGroup(taskGroup, params).then(
        (response) => {
          // success toast will now be called by task context group listener
          if (response.status !== "success") {
            toast({
              title: response.message,
              description: response.details,
              status: "error",
            });
          }
        }
      );
    },
    [toast]
  );

  const handleCancelProgressiveTaskGroup = useCallback(
    (taskGroup: string) => {
      TaskService.cancelProgressiveTaskGroup(taskGroup).then((response) => {
        if (response.status !== "success") {
          toast({
            title: response.message,
            description: response.details,
            status: "error",
          });
        }
      });
    },
    [toast]
  );

  const handleStopProgressiveTaskGroup = useCallback(
    (taskGroup: string) => {
      TaskService.stopProgressiveTaskGroup(taskGroup).then((response) => {
        if (response.status !== "success") {
          toast({
            title: response.message,
            description: response.details,
            status: "error",
          });
        }
      });
    },
    [toast]
  );

  const handleResumeProgressiveTaskGroup = useCallback(
    (taskGroup: string) => {
      TaskService.resumeProgressiveTaskGroup(taskGroup).then((response) => {
        if (response.status !== "success") {
          toast({
            title: response.message,
            description: response.details,
            status: "error",
          });
        }
      });
    },
    [toast]
  );

  const isActiveGroup = (t: TaskGroupDesc) =>
    t.status === GTaskEventStatusEnums.Started ||
    t.status === GTaskEventStatusEnums.Stopped;

  const handleClearHistoryTaskGroups = useCallback(() => {
    setTasks((prev) => {
      prev
        .filter((t) => !isActiveGroup(t))
        .forEach((group) => {
          TaskService.deleteProgressiveTaskGroup(group.taskGroup);
        });
      return prev.filter(isActiveGroup);
    });
  }, []);

  useEffect(() => {
    const unlisten = TaskService.onProgressiveTaskUpdate(
      (payload: PTaskEventPayload) => {
        // info(
        //   `Received task update: ${payload.id}, status: ${payload.event.status}`
        // );
        setTasks((prevTasks) => {
          const group = prevTasks?.find(
            (t) => t.taskGroup === payload.taskGroup
          );

          switch (payload.event.status) {
            case PTaskEventStatusEnums.Created: {
              if (group) {
                if (group.taskDescs.some((t) => t.taskId === payload.id)) {
                  // info(
                  //   `Task ${payload.id} already exists in group ${payload.taskGroup}`
                  // );
                } else if (
                  destOf(
                    (payload.event as CreatedPTaskEventStatus).desc.payload
                  ) !== undefined &&
                  group.taskDescs.some(
                    (t) =>
                      destOf(t.payload) ===
                      destOf(
                        (payload.event as CreatedPTaskEventStatus).desc.payload
                      )
                  )
                ) {
                  // It' a retrial task emitted from the backend
                  group.taskDescs = group.taskDescs.map((t) => {
                    if (
                      destOf(t.payload) ===
                      destOf(
                        (payload.event as CreatedPTaskEventStatus).desc.payload
                      )
                    ) {
                      t = (payload.event as CreatedPTaskEventStatus).desc;
                    }
                    return t;
                  });
                } else {
                  group.taskDescs.unshift(payload.event.desc);
                  // info(`Added task ${payload.id} to group ${payload.taskGroup}`);
                  updateGroupInfo(group);
                }
              } else {
                // info(`Creating new task group ${payload.taskGroup}`);
                // Create a new task group if it doesn't exist
                let newGroup: TaskGroupDesc = {
                  status: GTaskEventStatusEnums.Started,
                  taskGroup: payload.taskGroup,
                  taskDescs: [payload.event.desc],
                };
                updateGroupInfo(newGroup);
                return [newGroup, ...(prevTasks || [])];
              }
              break;
            }

            case PTaskEventStatusEnums.Started: {
              if (!group) return prevTasks;
              group.taskDescs = group.taskDescs.map((t) => {
                if (t.taskId === payload.id) {
                  t.status = TaskDescStatusEnums.InProgress;
                  t.total = (payload.event as StartedPTaskEventStatus).total;
                }
                return t;
              });
              updateGroupInfo(group);
              break;
            }

            case PTaskEventStatusEnums.Completed: {
              if (!group) return prevTasks;
              group.taskDescs = group.taskDescs.map((t) => {
                if (t.taskId === payload.id) {
                  t.status = TaskDescStatusEnums.Completed;
                  t.current = t.total;
                }
                return t;
              });
              // info(`Task ${payload.id} completed in group ${payload.taskGroup}`);
              updateGroupInfo(group);
              break;
            }

            case PTaskEventStatusEnums.Stopped: {
              if (!group) return prevTasks;
              group.taskDescs = group.taskDescs.map((t) => {
                if (t.taskId === payload.id) {
                  t.status = TaskDescStatusEnums.Stopped;
                }
                return t;
              });
              updateGroupInfo(group);
              break;
            }

            case PTaskEventStatusEnums.Cancelled: {
              if (!group) return prevTasks;
              group.taskDescs = group.taskDescs.map((t) => {
                if (t.taskId === payload.id) {
                  t.status = TaskDescStatusEnums.Cancelled;
                }
                return t;
              });
              updateGroupInfo(group);
              // info(`Task ${payload.id} cancelled in group ${payload.taskGroup}`);
              break;
            }

            case PTaskEventStatusEnums.InProgress: {
              if (!group) return prevTasks;
              group.taskDescs = group.taskDescs.map((t) => {
                if (t.taskId === payload.id) {
                  t.current = (
                    payload.event as InProgressPTaskEventStatus
                  ).current;
                  t.status = TaskDescStatusEnums.InProgress;
                  t.estimatedTime = (
                    payload.event as InProgressPTaskEventStatus
                  ).estimatedTime;
                  t.speed = (payload.event as InProgressPTaskEventStatus).speed;
                }
                return t;
              });
              updateGroupInfo(group);
              // info(
              //   `Task ${payload.id} in progress in group ${payload.taskGroup}`
              // );
              break;
            }

            case PTaskEventStatusEnums.Failed: {
              logger.error(
                `Task ${payload.id} failed in group ${payload.taskGroup}: ${
                  (payload.event as FailedPTaskEventStatus).reason
                }`
              );
              if (!group) return prevTasks;
              group.taskDescs = group.taskDescs.map((t) => {
                if (t.taskId === payload.id) {
                  t.status = TaskDescStatusEnums.Failed;
                  t.reason = (payload.event as FailedPTaskEventStatus).reason;
                }
                return t;
              });
              updateGroupInfo(group);
              // info(`Task ${payload.id} failed in group ${payload.taskGroup}`);
              break;
            }

            default:
              break;
          }

          return [...prevTasks];
        });
      }
    );

    return () => {
      unlisten();
    };
  }, [t, toast, updateGroupInfo]);

  useEffect(() => {
    const unlisten = TaskService.onTaskGroupUpdate(
      (payload: GTaskEventPayload) => {
        logger.info(`Received task group update: ${payload.event}`);
        setTasks((prevTasks) => {
          let newTasks = prevTasks.map((task) => {
            if (task.taskGroup === payload.taskGroup) {
              task.status = payload.event;
              if (payload.event === GTaskEventStatusEnums.Completed) {
                task.taskDescs.forEach((t) => {
                  if (
                    t.status === TaskDescStatusEnums.Waiting ||
                    t.status === TaskDescStatusEnums.InProgress
                  ) {
                    t.status = TaskDescStatusEnums.Completed;
                    t.current = t.total;
                  } else if (t.status === TaskDescStatusEnums.Failed) {
                    task.status = GTaskEventStatusEnums.Failed;
                    payload.event = GTaskEventStatusEnums.Failed;
                  }
                });
              }
            }
            return task;
          });

          const { name, version } = parseTaskGroup(payload.taskGroup);

          if (
            (name === "loader-installer" || name === "optifine-installer") &&
            payload.event === GTaskEventStatusEnums.Completed &&
            version &&
            !loaderPreparationsClaimed.current.has(payload.taskGroup)
          ) {
            const instanceId = version;
            const component =
              name === "optifine-installer" ? "optifine" : "loader";
            loaderPreparationsClaimed.current.add(payload.taskGroup);
            InstanceService.prepareModLoaderInstall(instanceId, component)
              .then((response) => {
                if (response.status !== "success") {
                  toast({
                    status: "error",
                    title: response.message,
                    description: response.details,
                  });
                  getInstanceList(true);
                }
              })
              .catch((error) => {
                logger.error(
                  "Failed to prepare mod loader after installer download:",
                  error
                );
                getInstanceList(true);
              });
          }

          // OptiFine may bundle every runtime dependency, leaving an empty
          // group: it has no Created task event to add its card to the list.
          if (
            name === "optifine-libraries" &&
            !newTasks.some((task) => task.taskGroup === payload.taskGroup)
          ) {
            newTasks = [
              ...newTasks,
              {
                taskGroup: payload.taskGroup,
                taskDescs: [],
                status: payload.event,
              },
            ];
          }

          // Modpack jobs report their own final result, independently of dialogs.
          // Installer-only groups continue into loader preparation silently.
          if (
            name !== "modpack-publish" &&
            name !== "modpack-install" &&
            name !== "modpack-update" &&
            name !== "loader-installer" &&
            name !== "optifine-installer"
          )
            toast({
              status:
                payload.event === GTaskEventStatusEnums.Failed
                  ? "error"
                  : "success",
              title: t(
                `Services.task.onTaskGroupUpdate.status.${payload.event}`,
                {
                  param: t(`DownloadTasksPage.task.${name}`, {
                    param: version || "",
                  }),
                }
              ),
            });

          if (payload.event === GTaskEventStatusEnums.Completed) {
            switch (name) {
              case "game-client":
              case "change-mod-loader": {
                getInstanceList(true);
                break;
              }
              case "mod":
              case "mod-update":
                emit("instance:refresh-resource-list", OtherResourceType.Mod);
                break;
              case "resourcepack":
                emit(
                  "instance:refresh-resource-list",
                  OtherResourceType.ResourcePack
                );
                break;
              case "shader":
                emit(
                  "instance:refresh-resource-list",
                  OtherResourceType.ShaderPack
                );
                break;
              case "launcher-update": {
                let group = newTasks.find(
                  (t) => t.taskGroup === payload.taskGroup
                );
                if (group && group.taskDescs.length > 0) {
                  ConfigService.installLauncherUpdate(
                    group.taskDescs[0].payload.filename || "",
                    true
                  ).then((response) => {
                    if (response.status !== "success") {
                      toast({
                        title: response.message,
                        description: response.details,
                        status: "error",
                      });
                    }
                  });
                }
                break;
              }
              case "mojang-java":
                getJavaInfos(true);
                break;
              default:
                break;
            }
          }

          return newTasks;
        });
      }
    );
    return () => {
      unlisten();
    };
  }, [
    t,
    toast,
    closeToast,
    getInstanceList,
    updateGroupInfo,
    getJavaInfos,
    openSharedModal,
  ]);

  const [unfinishedPublishes, setUnfinishedPublishes] = useState<
    CampusUnfinishedPublish[]
  >([]);

  // Keep the native listener mounted across list refreshes and navigation.
  const updateFinishedHandler = React.useRef<
    (payload: ModpackUpdateFinished) => void
  >(() => {});
  updateFinishedHandler.current = (payload) => {
    setUpdateResults((previous) => ({
      ...previous,
      [payload.group]: payload,
    }));
    finishModpackUpdate(payload, {
      refresh: () => {
        getInstanceList(true);
      },
      check: checkModpackUpdate,
      notify: (failed, error) =>
        toast({
          title: t(
            failed
              ? "Services.instance.applyModpackUpdate.error.title"
              : "Services.instance.applyModpackUpdate.success"
          ),
          description: error
            ? localizeModpackUpdateError(t, error)
            : payload.instanceName,
          status: failed ? "error" : "success",
        }),
      loaderPending: () => {
        toast({
          title: t("UpdateModpackModal.toast.loaderPending"),
          status: "info",
        });
        void router.push("/downloads");
      },
    });
  };
  useEffect(
    () =>
      ModpackUpdateService.onUpdateFinished((payload) =>
        updateFinishedHandler.current(payload)
      ),
    []
  );
  const refreshUnfinishedPublishes = useCallback(() => {
    CampusModpackService.listCampusModpackPublishJobs().then((response) => {
      if (response.status === "success") setUnfinishedPublishes(response.data);
    });
  }, []);

  const refreshUnfinishedInstalls = useCallback(() => {
    CampusModpackService.listCampusModpackInstallJobs().then((response) => {
      if (response.status === "success") setUnfinishedInstalls(response.data);
    });
  }, []);

  useEffect(
    () =>
      CampusModpackService.onInstallGroupsRemoved((groups) => {
        setTasks((previous) => removeCampusInstallGroups(previous, groups));
        setInstallProgress((previous) =>
          Object.fromEntries(
            Object.entries(previous).filter(
              ([group]) => !groups.includes(group)
            )
          )
        );
      }),
    []
  );

  useEffect(
    () =>
      CampusModpackService.onInstallProgress((progress) => {
        if (progress.group) {
          setInstallProgress((previous) => ({
            ...previous,
            [progress.group!]: progress,
          }));
        }
      }),
    []
  );

  useEffect(
    () =>
      CampusModpackService.onInstallFinished((payload) => {
        refreshUnfinishedInstalls();
        if (!payload.error) getInstanceList(true);
        toast({
          title: t(
            payload.error
              ? "Services.instance.installCampusModpack.error.title"
              : "Services.instance.installCampusModpack.success"
          ),
          description: payload.error
            ? localizeCampusInstallError(t, payload.error)
            : undefined,
          status: payload.error ? "error" : "success",
        });
      }),
    [getInstanceList, refreshUnfinishedInstalls, t, toast]
  );

  // The records drive the nav-bar task button too, so they are fetched here
  // once, on every publish ending, and on a slow timer — the components
  // showing them never poll on their own (§3.2.2, 10-23 addition).
  useEffect(() => {
    refreshUnfinishedPublishes();
    refreshUnfinishedInstalls();
    const timer = setInterval(() => {
      refreshUnfinishedPublishes();
      refreshUnfinishedInstalls();
    }, 3000);
    return () => clearInterval(timer);
  }, [refreshUnfinishedInstalls, refreshUnfinishedPublishes]);

  // A publish runs in the background; this is where its result is told.
  useEffect(() => {
    return CampusModpackService.onPublishFinished((payload) => {
      refreshUnfinishedPublishes();
      if (payload.error !== undefined) {
        toast({
          title: t("Services.instance.publishCampusModpack.error.title"),
          description: t(
            `Services.instance.publishCampusModpack.error.description.${payload.error}`,
            { defaultValue: payload.error }
          ),
          status: "error",
        });
        return;
      }
      const adoption = payload.outcome?.adoption;
      if (adoption === "followed") getInstanceList(true);
      const skipped = payload.outcome?.skipped.length ?? 0;
      const locallyDeleted = payload.outcome?.locallyDeleted ?? 0;
      const notes = [
        skipped > 0 ? t("PublishModpackModal.skipped", { count: skipped }) : "",
        adoption && adoption !== "followed"
          ? t(`PublishModpackModal.adoption.${adoption}`)
          : locallyDeleted > 0
            ? t("PublishModpackModal.adoption.localDeleted", {
                count: locallyDeleted,
              })
            : "",
        adoption && adoption !== "followed" && locallyDeleted > 0
          ? t("PublishModpackModal.adoption.localDeleted", {
              count: locallyDeleted,
            })
          : "",
      ].filter(Boolean);
      toast({
        title: t("Services.instance.publishCampusModpack.success"),
        description: notes.length > 0 ? notes.join(" ") : undefined,
        status: "success",
      });
    });
  }, [getInstanceList, refreshUnfinishedPublishes, t, toast]);

  useEffect(() => {
    if (!tasks || !tasks.length) setGeneralPercent(undefined);
    else {
      let filteredTasks = tasks.filter(
        (t) => t.status === GTaskEventStatusEnums.Started
      );

      if (filteredTasks.length === 0) setGeneralPercent(undefined);
      else {
        setGeneralPercent(
          filteredTasks.reduce(
            (acc, group) => acc + (group.progress ?? 0) / filteredTasks.length,
            0
          )
        );
      }
    }
  }, [tasks]);

  return (
    <TaskContext.Provider
      value={{
        tasks,
        installProgress,
        updateResults,
        generalPercent,
        unfinishedPublishes,
        refreshUnfinishedPublishes,
        unfinishedInstalls,
        refreshUnfinishedInstalls,
        handleScheduleProgressiveTaskGroup,
        handleCancelProgressiveTaskGroup,
        handleStopProgressiveTaskGroup,
        handleResumeProgressiveTaskGroup,
        handleClearHistoryTaskGroups,
      }}
    >
      {children}
    </TaskContext.Provider>
  );
};

export const useTaskContext = (): TaskContextType => {
  const context = useContext(TaskContext);
  if (!context) {
    throw new Error("useTaskContext must be used within a TaskContextProvider");
  }
  return context;
};

/** Where a download writes to; other kinds of task have no destination. */
export const destOf = (payload: TaskPayload): string | undefined =>
  payload.taskType === TaskTypeEnums.Download ? payload.dest : undefined;

export const parseTaskGroup = (
  taskGroup: string
): {
  name: string;
  version?: string;
  timestamp: number;
  isRetry: boolean;
  rawName: string;
} => {
  const lastAtIndex = taskGroup.lastIndexOf("@");
  let rawName: string;
  let timestamp: number;

  if (lastAtIndex === -1) {
    rawName = taskGroup;
    timestamp = Date.now();
  } else {
    rawName = taskGroup.substring(0, lastAtIndex);
    timestamp = parseInt(taskGroup.substring(lastAtIndex + 1));
  }

  const [name, version] = rawName.split("?");

  return {
    name: name.replace(/^retry-/, ""),
    version,
    isRetry: name.startsWith("retry-"),
    timestamp: timestamp,
    rawName,
  };
};
