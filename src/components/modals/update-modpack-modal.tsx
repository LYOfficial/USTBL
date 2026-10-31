import {
  Alert,
  AlertIcon,
  Badge,
  Box,
  Button,
  ButtonGroup,
  HStack,
  Icon,
  Modal,
  ModalBody,
  ModalCloseButton,
  ModalContent,
  ModalFooter,
  ModalHeader,
  ModalOverlay,
  ModalProps,
  Progress,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { LuChevronDown, LuChevronRight } from "react-icons/lu";
import { DiffFile, DiffHunks, regionHunk } from "@/components/common/diff-view";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import { InstanceError } from "@/enums/service-error";
import { CampusInstallProgress } from "@/models/campus-modpack";
import {
  ModpackConflictChoice,
  ModpackFileChange,
  ModpackUpdateCheck,
  ModpackUpdatePlan,
} from "@/models/modpack-update";
import { CampusModpackService } from "@/services/campus-modpack";
import { ModpackUpdateService } from "@/services/modpack-update";
import {
  failedUpdatePlans,
  rememberedChoices,
  resolvePlanChoices,
} from "@/utils/modpack-update-choices";

interface UpdateModpackModalProps extends Omit<ModalProps, "children"> {
  instanceId: string;
  instanceName: string;
}

type CheckPhase = "checking" | "checked" | "applying";

type PlanPhase = "idle" | "planning" | "planned" | "planFailed";

interface PlanError {
  message: string;
  details?: string;
}

const formatSize = (size?: number) => {
  if (size === undefined) return "";
  if (size < 1024) return `${size} B`;
  const units = ["KiB", "MiB", "GiB"];
  let value = size / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[unit]}`;
};

const fileName = (path: string) => path.split("/").pop() ?? path;

/** One collapsible group of file changes, capped until "show all". */
const ChangeGroup: React.FC<{
  title: string;
  colorScheme: string;
  changes: ModpackFileChange[];
  renderSize: (change: ModpackFileChange) => string;
}> = ({ title, colorScheme, changes, renderSize }) => {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [showAll, setShowAll] = useState(false);
  if (changes.length === 0) return null;
  const sorted = [...changes].sort((a, b) => a.path.localeCompare(b.path));
  const shown = showAll ? sorted : sorted.slice(0, 100);
  return (
    <Box borderWidth="1px" borderRadius="md">
      <HStack
        as="button"
        type="button"
        w="full"
        px={3}
        py={2}
        spacing={2}
        onClick={() => setOpen(!open)}
        aria-expanded={open}
      >
        <Icon as={open ? LuChevronDown : LuChevronRight} />
        <Text fontWeight="medium" color={`${colorScheme}.500`}>
          {t(title, { count: changes.length })}
        </Text>
      </HStack>
      {open && (
        <VStack align="stretch" spacing={0} borderTopWidth="1px">
          {shown.map((change) => (
            <HStack
              key={change.path}
              justify="space-between"
              spacing={3}
              px={3}
              py={1.5}
              borderTopWidth="1px"
            >
              <Box minW={0}>
                <Text fontSize="sm" noOfLines={1} wordBreak="break-all">
                  {fileName(change.path)}
                </Text>
                {change.path.includes("/") && (
                  <Text fontSize="xs" className="secondary-text" noOfLines={1}>
                    {change.path}
                  </Text>
                )}
              </Box>
              <Text fontSize="xs" className="secondary-text" flexShrink={0}>
                {renderSize(change)}
              </Text>
            </HStack>
          ))}
          {!showAll && sorted.length > shown.length && (
            <Button
              variant="ghost"
              size="xs"
              onClick={() => setShowAll(true)}
              borderTopWidth="1px"
              borderRadius={0}
            >
              {t("UpdateModpackModal.plan.showAll", {
                count: sorted.length - shown.length,
              })}
            </Button>
          )}
        </VStack>
      )}
    </Box>
  );
};

const UpdateModpackModal: React.FC<UpdateModpackModalProps> = ({
  instanceId,
  instanceName,
  ...modalProps
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const toast = useToast();
  const { getInstanceList, checkModpackUpdate } = useGlobalData();
  const primaryColor = config.appearance.theme.primaryColor;

  const [checkPhase, setCheckPhase] = useState<CheckPhase>("checking");
  const [planPhase, setPlanPhase] = useState<PlanPhase>("planning");
  const [check, setCheck] = useState<ModpackUpdateCheck | null>(null);
  const [plan, setPlan] = useState<ModpackUpdatePlan | null>(null);
  const [choices, setChoices] = useState<Record<string, ModpackConflictChoice>>(
    {}
  );
  const [progress, setProgress] = useState<CampusInstallProgress | null>(null);
  const [confirmingRollback, setConfirmingRollback] = useState(false);
  const [planError, setPlanError] = useState<PlanError | null>(null);
  const choicesRef = useRef(choices);
  choicesRef.current = choices;
  // Each run of the open/retry flow gets a token; a preview that answers to
  // an older token is stale (cancelled by the check or a retry) and dropped.
  const roundRef = useRef(0);
  // After a failed apply the choices stay until the re-check proves the plan
  // still is the one they were made for. The failed plan's hash travels in a
  // ref: runCheck clears the displayed plan before the new prepare answers.
  const keepChoicesForRecheckRef = useRef(false);
  const failedPlanHashRef = useRef<string | null>(null);
  // Synchronous in-flight guards so two clicks inside the same render frame
  // cannot both enter the backend. State updates alone are async and leave a
  // window for double submission.
  const applyInFlight = useRef(false);
  const checkInFlight = useRef(false);
  const rollbackInFlight = useRef(false);
  const resumeInFlight = useRef(false);
  const prepareInFlight = useRef(false);

  const preparePlan = useCallback(
    async (round: number) => {
      if (prepareInFlight.current) return;
      prepareInFlight.current = true;
      try {
        const response =
          await ModpackUpdateService.prepareModpackUpdate(instanceId);
        if (roundRef.current !== round) return;
        if (response.status === "success") {
          const recheck = keepChoicesForRecheckRef.current;
          keepChoicesForRecheckRef.current = false;
          const failedPlanHash = failedPlanHashRef.current;
          failedPlanHashRef.current = null;
          failedUpdatePlans.delete(instanceId);
          const resolved = resolvePlanChoices({
            instanceId,
            newPlanHash: response.data.planHash,
            failedPlanHash,
            recheckAfterFailure: recheck,
            remembered: rememberedChoices,
            currentChoices: choicesRef.current,
          });
          setChoices(resolved.choices);
          if (resolved.toastPlanChanged) {
            toast({
              title: t("UpdateModpackModal.toast.planChanged"),
              status: "info",
            });
          }
          setPlan(response.data);
          setPlanPhase("planned");
        } else if (
          response.details !== InstanceError.ModpackUpdatePrepareCancelled
        ) {
          setPlanError({
            message: response.message,
            details: response.details,
          });
          setPlanPhase("planFailed");
        } else {
          setPlanPhase("idle");
        }
      } finally {
        prepareInFlight.current = false;
      }
    },
    [instanceId, t, toast]
  );

  const runCheck = useCallback(async () => {
    if (checkInFlight.current) return;
    checkInFlight.current = true;
    try {
      const round = ++roundRef.current;
      const failedHash = failedUpdatePlans.get(instanceId);
      if (failedHash) {
        keepChoicesForRecheckRef.current = true;
        failedPlanHashRef.current = failedHash;
        choicesRef.current = rememberedChoices.get(instanceId)?.choices ?? {};
        setChoices(choicesRef.current);
      }
      setCheckPhase("checking");
      setPlanPhase("planning");
      setPlan(null);
      setPlanError(null);
      if (!keepChoicesForRecheckRef.current) {
        setChoices({});
      }
      setConfirmingRollback(false);
      // A campus installation belongs to the task page, never the update planner.
      if (
        getInstanceList()?.find((instance) => instance.id === instanceId)
          ?.campusInstallPending
      ) {
        setCheck(await checkModpackUpdate(instanceId));
        setCheckPhase("checked");
        setPlanPhase("idle");
        return;
      }
      // An interrupted update is rolled back before anything is compared.
      const recovered =
        await ModpackUpdateService.recoverModpackUpdate(instanceId);
      if (
        recovered.status !== "success" &&
        recovered.raw_error === InstanceError.ModpackUpdateBusy
      ) {
        setCheck({
          status: "unknown",
          follow: true,
          canRollBack: false,
          reason: InstanceError.ModpackUpdateBusy,
        });
        setCheckPhase("checked");
        setPlanPhase("idle");
        return;
      }
      if (recovered.status === "success" && recovered.data) {
        getInstanceList(true);
      }
      // Check and preview run at the same time: the preview hits its cache
      // when nothing changed, and the check is the light listing request.
      const preparePromise = preparePlan(round);
      const result = await checkModpackUpdate(instanceId, false);
      if (roundRef.current !== round) return;
      setCheck(result);
      setCheckPhase("checked");
      if (result.status !== "available") {
        // No update: the preview is not worth waiting for; its answer, whenever
        // it comes, belongs to a finished round and is dropped.
        roundRef.current += 1;
        void ModpackUpdateService.cancelModpackUpdatePrepare(instanceId);
        setPlanPhase("idle");
        if (result.status === "upToDate") {
          // Only now is the local scan worth its hashing.
          const scanned = await checkModpackUpdate(instanceId, true);
          setCheck(scanned);
        }
      }
      await preparePromise;
    } finally {
      checkInFlight.current = false;
    }
  }, [instanceId, getInstanceList, checkModpackUpdate, preparePlan]);

  // The opening check runs once per instance. runCheck also depends on
  // getInstanceList, whose identity changes on every instance list update
  // (e.g. play time writes), so it must not retrigger this effect: that
  // would restart recover + scan + prepare and discard the choices.
  const runCheckRef = useRef(runCheck);
  runCheckRef.current = runCheck;
  useEffect(() => {
    void runCheckRef.current();
  }, [instanceId]);

  // Save before submitting: even a completion before the acknowledgement can
  // clear these, without a later unmount writing stale choices back.
  useEffect(() => {
    if (plan && checkPhase !== "applying") {
      rememberedChoices.set(instanceId, {
        planHash: plan.planHash,
        choices,
      });
    }
  }, [instanceId, plan, choices, checkPhase]);

  useEffect(() => {
    return CampusModpackService.onInstallProgress((payload) => {
      if (payload.instanceId === instanceId) setProgress(payload);
    });
  }, [instanceId]);

  const handleApply = useCallback(async () => {
    if (!plan || applyInFlight.current) return;
    applyInFlight.current = true;
    try {
      rememberedChoices.set(instanceId, { planHash: plan.planHash, choices });
      setProgress(null);
      setCheckPhase("applying");
      // Take the plan off the screen so its buttons stop rendering.
      setPlanPhase("idle");
      const response = await ModpackUpdateService.applyModpackUpdate(
        instanceId,
        plan.planHash,
        choices
      );
      if (response.status === "success") {
        toast({
          title: t("DownloadTasksPage.updateStarted"),
          description: instanceName,
          status: "info",
        });
        modalProps.onClose();
      } else {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
        // The plan may be stale or rolled back; start over from a fresh check.
        // The choices stay for the re-check: it clears them itself when the
        // plan turns out to be another one (and says so).
        keepChoicesForRecheckRef.current = true;
        failedPlanHashRef.current = plan.planHash;
        runCheck();
      }
    } finally {
      applyInFlight.current = false;
    }
  }, [plan, instanceId, choices, instanceName, toast, t, modalProps, runCheck]);

  const handleRollback = useCallback(async () => {
    if (rollbackInFlight.current) return;
    rollbackInFlight.current = true;
    try {
      setCheckPhase("applying");
      setPlanPhase("idle");
      setProgress(null);
      const response =
        await ModpackUpdateService.rollbackModpackUpdate(instanceId);
      getInstanceList(true);
      if (response.status === "success") {
        toast({
          title: response.message,
          status: response.data ? "success" : "info",
        });
      } else {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
      runCheck();
    } finally {
      rollbackInFlight.current = false;
    }
  }, [instanceId, getInstanceList, runCheck, toast]);

  const revealConflictFile = useCallback(
    async (path: string) => {
      const response = await ModpackUpdateService.revealModpackConflictFile(
        instanceId,
        path
      );
      if (response.status !== "success") {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
    },
    [instanceId, toast]
  );

  /** Answer every conflict, block by block where a file has blocks. */
  const chooseAll = useCallback(
    (choice: ModpackConflictChoice) => {
      if (!plan) return;
      const next: Record<string, ModpackConflictChoice> = {};
      for (const conflict of plan.conflicts) {
        // What the player ignored stays with them, whatever "all" says.
        const skipTake = choice === "takeRemote";
        if (conflict.hunks.length === 0) {
          next[conflict.path] =
            skipTake && conflict.ignored ? "keepLocal" : choice;
        } else {
          conflict.hunks.forEach((_, index) => {
            next[`${conflict.path}#${index}`] =
              skipTake && conflict.ignoredHunks?.includes(index)
                ? "keepLocal"
                : choice;
          });
        }
      }
      setChoices(next);
    },
    [plan]
  );

  const renderChoice = (key: string, choice: ModpackConflictChoice) => (
    <ButtonGroup size="xs" isAttached variant="outline" flexShrink={0}>
      {(["keepLocal", "takeRemote"] as const).map((option) => (
        <Button
          key={option}
          colorScheme={choice === option ? primaryColor : undefined}
          variant={choice === option ? "solid" : "outline"}
          aria-pressed={choice === option}
          onClick={() => setChoices((prev) => ({ ...prev, [key]: option }))}
        >
          {t(`UpdateModpackModal.choice.${option}`)}
        </Button>
      ))}
    </ButtonGroup>
  );

  const handleResume = useCallback(async () => {
    if (resumeInFlight.current) return;
    resumeInFlight.current = true;
    try {
      const response = await ModpackUpdateService.setModpackFollow(
        instanceId,
        true
      );
      if (response.status === "success") {
        getInstanceList(true);
        runCheck();
      } else {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
    } finally {
      resumeInFlight.current = false;
    }
  }, [instanceId, getInstanceList, runCheck, toast]);

  const reasonText = (code?: string) =>
    code
      ? t(`UpdateModpackModal.reason.${code}`, { defaultValue: code })
      : undefined;

  const renderChecked = () => {
    if (!check) return null;
    switch (check.status) {
      case "available":
        return (
          <VStack align="start" spacing={2}>
            <Text>
              {t("UpdateModpackModal.status.available", {
                latest: check.latestLabel,
              })}
            </Text>
            {check.currentLabel && (
              <Text fontSize="xs" className="secondary-text">
                {t("UpdateModpackModal.label.current", {
                  current: check.currentLabel,
                })}
              </Text>
            )}
          </VStack>
        );
      case "upToDate":
        return <Text>{t("UpdateModpackModal.status.upToDate")}</Text>;
      case "localChanges":
        return (
          <VStack align="start" spacing={1}>
            <Text>{t("UpdateModpackModal.status.upToDate")}</Text>
            <Text fontSize="xs" className="secondary-text">
              {t("UpdateModpackModal.status.localChanges", {
                count: check.localChanges ?? 0,
              })}
            </Text>
          </VStack>
        );
      case "waitingLoader":
        return <Text>{t("UpdateModpackModal.status.waitingLoader")}</Text>;
      case "installing":
        return <Text>{t("InstanceSyncPage.ledger.badge.installing")}</Text>;
      case "paused":
        return <Text>{t("UpdateModpackModal.status.paused")}</Text>;
      case "notFollowing":
        return <Text>{t("UpdateModpackModal.status.notFollowing")}</Text>;
      case "needsRepair":
      case "unknown":
        return (
          <Alert status={check.status === "unknown" ? "warning" : "error"}>
            <AlertIcon />
            <VStack align="start" spacing={1}>
              <Text>{t(`UpdateModpackModal.status.${check.status}`)}</Text>
              {check.reason && (
                <Text fontSize="xs">{reasonText(check.reason)}</Text>
              )}
            </VStack>
          </Alert>
        );
    }
  };

  const renderPlan = () => {
    if (!plan) return null;
    if (plan.reinstallRequired) {
      return (
        <Alert status="warning">
          <AlertIcon />
          {t("UpdateModpackModal.plan.reinstallRequired", {
            latest: plan.toLabel,
          })}
        </Alert>
      );
    }
    if (plan.fastForward) {
      return (
        <Text>
          {t("UpdateModpackModal.plan.fastForward", { latest: plan.toLabel })}
        </Text>
      );
    }
    const adds = plan.changes.filter((c) => c.kind === "add");
    const updates = plan.changes.filter((c) => c.kind === "update");
    const deletes = plan.changes.filter((c) => c.kind === "delete");
    const disables = plan.changes.filter((c) => c.kind === "disable");
    const enables = plan.changes.filter((c) => c.kind === "enable");
    const retires = plan.changes.filter((c) => c.kind === "retire");
    return (
      <VStack align="stretch" spacing={3}>
        <Text>
          {t("UpdateModpackModal.plan.summary", { latest: plan.toLabel })}
        </Text>
        {plan.loaderChange && (
          <Text fontSize="xs-sm">
            {t("UpdateModpackModal.plan.loaderChange", {
              from: plan.loaderChange.from.version,
              to: plan.loaderChange.to.version,
            })}
          </Text>
        )}
        <VStack align="stretch" spacing={2}>
          <ChangeGroup
            title="UpdateModpackModal.plan.add"
            colorScheme="green"
            changes={adds}
            renderSize={(change) => formatSize(change.remote?.size)}
          />
          <ChangeGroup
            title="UpdateModpackModal.plan.update"
            colorScheme="blue"
            changes={updates}
            renderSize={(change) =>
              `${formatSize(change.localSize)} → ${formatSize(change.remote?.size)}`
            }
          />
          <ChangeGroup
            title="UpdateModpackModal.plan.delete"
            colorScheme="red"
            changes={deletes}
            renderSize={(change) => formatSize(change.localSize)}
          />
          <ChangeGroup
            title="UpdateModpackModal.plan.disable"
            colorScheme="gray"
            changes={disables}
            renderSize={(change) => formatSize(change.localSize)}
          />
          <ChangeGroup
            title="UpdateModpackModal.plan.enable"
            colorScheme="gray"
            changes={enables}
            renderSize={(change) => formatSize(change.localSize)}
          />
          <ChangeGroup
            title="UpdateModpackModal.plan.retire"
            colorScheme="gray"
            changes={retires}
            renderSize={(change) => formatSize(change.localSize)}
          />
          <ChangeGroup
            title="UpdateModpackModal.plan.autoMerged"
            colorScheme="green"
            changes={plan.autoMerged}
            renderSize={(change) =>
              `${formatSize(change.localSize)} → ${formatSize(change.remote?.size)}`
            }
          />
        </VStack>
        {plan.conflicts.length > 0 && (
          <VStack align="stretch" spacing={3}>
            <HStack justify="space-between" spacing={2}>
              <Text fontWeight="medium">
                {t("UpdateModpackModal.plan.conflicts", {
                  count: plan.conflicts.length,
                })}
              </Text>
              <HStack spacing={1} flexShrink={0}>
                <Button
                  size="xs"
                  variant="ghost"
                  onClick={() => chooseAll("keepLocal")}
                >
                  {t("UpdateModpackModal.choice.allKeepLocal")}
                </Button>
                <Button
                  size="xs"
                  variant="ghost"
                  onClick={() => chooseAll("takeRemote")}
                >
                  {t("UpdateModpackModal.choice.allTakeRemote")}
                </Button>
              </HStack>
            </HStack>
            {plan.conflicts.some(
              (conflict) =>
                conflict.ignored || (conflict.ignoredHunks?.length ?? 0) > 0
            ) && (
              <Text fontSize="xs" className="secondary-text">
                {t("UpdateModpackModal.ignored.takeAllNote")}
              </Text>
            )}
            {plan.conflicts.map((conflict) => {
              // A text conflict is answered block by block; anything else as
              // a whole file. Never both, so each answer has one control.
              const byHunk = conflict.hunks.length > 0;
              return (
                <DiffFile
                  key={conflict.path}
                  path={conflict.path}
                  tags={
                    <Text
                      fontSize="xs"
                      className="secondary-text"
                      noOfLines={1}
                    >
                      {t(`UpdateModpackModal.conflictKind.${conflict.kind}`)}
                    </Text>
                  }
                  actions={
                    <>
                      <Button
                        size="xs"
                        variant="ghost"
                        onClick={() => revealConflictFile(conflict.path)}
                      >
                        {t("UpdateModpackModal.choice.revealFile")}
                      </Button>
                      {!byHunk &&
                        renderChoice(
                          conflict.path,
                          choices[conflict.path] ?? "keepLocal"
                        )}
                    </>
                  }
                >
                  {conflict.ignored && (
                    <Text
                      fontSize="xs"
                      className="secondary-text"
                      px={3}
                      py={1.5}
                    >
                      {t("UpdateModpackModal.ignored.file")}
                    </Text>
                  )}
                  {byHunk && (
                    <VStack align="stretch" spacing={0}>
                      {conflict.hunks.map((hunk, index) => {
                        const key = `${conflict.path}#${index}`;
                        const choice = choices[key] ?? "keepLocal";
                        return (
                          <VStack
                            key={key}
                            align="stretch"
                            spacing={0}
                            borderTopWidth={index > 0 ? "1px" : 0}
                          >
                            <HStack
                              justify="space-between"
                              px={3}
                              py={1.5}
                              spacing={2}
                            >
                              <HStack spacing={2} minW={0}>
                                <Text fontSize="xs" className="secondary-text">
                                  {t("UpdateModpackModal.choice.hunk", {
                                    number: index + 1,
                                    from: hunk.baseStart + 1,
                                    to: Math.max(
                                      hunk.baseEnd,
                                      hunk.baseStart + 1
                                    ),
                                  })}
                                </Text>
                                {conflict.ignoredHunks?.includes(index) && (
                                  <Badge flexShrink={0}>
                                    {t("UpdateModpackModal.ignored.hunk")}
                                  </Badge>
                                )}
                              </HStack>
                              {renderChoice(key, choice)}
                            </HStack>
                            {(["keepLocal", "takeRemote"] as const).map(
                              (side) => (
                                <Box
                                  key={side}
                                  opacity={choice === side ? 1 : 0.55}
                                >
                                  <Text
                                    fontSize="xs"
                                    fontWeight="medium"
                                    px={3}
                                    pt={1}
                                  >
                                    {t(
                                      side === "keepLocal"
                                        ? "UpdateModpackModal.choice.localSide"
                                        : "UpdateModpackModal.choice.remoteSide"
                                    )}
                                  </Text>
                                  <DiffHunks
                                    hideHunkHeaders
                                    hunks={[
                                      regionHunk(
                                        hunk.baseStart + 1,
                                        hunk.base,
                                        side === "keepLocal"
                                          ? hunk.local
                                          : hunk.remote
                                      ),
                                    ]}
                                  />
                                </Box>
                              )
                            )}
                          </VStack>
                        );
                      })}
                    </VStack>
                  )}
                </DiffFile>
              );
            })}
          </VStack>
        )}
      </VStack>
    );
  };

  const renderApplying = () => (
    <VStack align="stretch" spacing={2}>
      <Text>{t("UpdateModpackModal.status.applying")}</Text>
      <Progress
        size="sm"
        colorScheme={primaryColor}
        isIndeterminate={
          !progress ||
          progress.total === 0 ||
          progress.stage === "preparing" ||
          progress.stage === "locating"
        }
        value={
          progress && progress.total > 0
            ? (progress.done / progress.total) * 100
            : 0
        }
        borderRadius="full"
      />
      {progress && (
        <Text fontSize="xs" className="secondary-text">
          {t(`Services.campusInstall.stage.${progress.stage}`, {
            done: progress.done,
            total: progress.total,
          })}
        </Text>
      )}
    </VStack>
  );

  const applying = checkPhase === "applying";
  const checking = checkPhase === "checking";
  // Waiting for the player: the actions on the instance are open.
  const idle =
    !applying &&
    !checking &&
    (planPhase === "idle" ||
      planPhase === "planned" ||
      planPhase === "planFailed");

  return (
    <Modal
      scrollBehavior="inside"
      // Diffs need the width; the rest of the flow stays compact.
      size={
        planPhase === "planned" && plan && plan.conflicts.length > 0
          ? { base: "2xl", lg: "4xl" }
          : { base: "lg", lg: "xl" }
      }
      autoFocus={false}
      closeOnOverlayClick={!applying}
      closeOnEsc={!applying}
      {...modalProps}
    >
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>
          {t("UpdateModpackModal.header.title", { name: instanceName })}
        </ModalHeader>
        {!applying && <ModalCloseButton />}
        <ModalBody>
          {(checking || planPhase === "planning") && (
            <Text className="secondary-text" mb={3}>
              {t(
                checking
                  ? "UpdateModpackModal.status.checking"
                  : "UpdateModpackModal.status.planning"
              )}
            </Text>
          )}
          {planPhase === "planning" && progress && (
            <Text className="secondary-text" fontSize="xs" mb={3}>
              {t(`Services.campusInstall.stage.${progress.stage}`, {
                done: progress.done,
                total: progress.total,
              })}
            </Text>
          )}
          {!checking && renderChecked()}
          {planPhase === "planFailed" && planError && (
            <Alert status="error" mt={3}>
              <AlertIcon />
              <VStack align="start" spacing={1}>
                <Text>{planError.message}</Text>
                {planError.details && (
                  <Text fontSize="xs">{reasonText(planError.details)}</Text>
                )}
              </VStack>
            </Alert>
          )}
          {planPhase === "planned" && renderPlan()}
          {idle && check?.canRollBack && (
            <VStack align="start" spacing={2} mt={4}>
              <Text fontSize="xs" className="secondary-text">
                {t("UpdateModpackModal.rollback.hint")}
              </Text>
              {confirmingRollback && (
                <Alert status="warning">
                  <AlertIcon />
                  {t("UpdateModpackModal.rollback.confirm")}
                </Alert>
              )}
            </VStack>
          )}
          {applying && renderApplying()}
        </ModalBody>
        <ModalFooter gap={2}>
          {idle && check?.canRollBack && (
            <Button
              variant="ghost"
              colorScheme={confirmingRollback ? "red" : undefined}
              onClick={() =>
                confirmingRollback
                  ? handleRollback()
                  : setConfirmingRollback(true)
              }
            >
              {t(
                confirmingRollback
                  ? "UpdateModpackModal.button.confirmRollback"
                  : "UpdateModpackModal.button.rollback"
              )}
            </Button>
          )}
          {idle && check?.status === "paused" && (
            <Button colorScheme={primaryColor} onClick={handleResume}>
              {t("UpdateModpackModal.button.resume")}
            </Button>
          )}
          {idle &&
            (check?.status === "unknown" ||
              check?.status === "needsRepair") && (
              <Button variant="ghost" onClick={runCheck}>
                {t("UpdateModpackModal.button.retry")}
              </Button>
            )}
          {!applying && planPhase === "planFailed" && (
            <Button
              colorScheme={primaryColor}
              onClick={() => {
                const round = ++roundRef.current;
                setPlanPhase("planning");
                setPlanError(null);
                void preparePlan(round);
              }}
            >
              {t("UpdateModpackModal.button.retry")}
            </Button>
          )}
          {!applying &&
            planPhase === "planned" &&
            plan &&
            !plan.reinstallRequired && (
              <Button colorScheme={primaryColor} onClick={handleApply}>
                {t(
                  plan.fastForward
                    ? "UpdateModpackModal.button.sync"
                    : "UpdateModpackModal.button.apply"
                )}
              </Button>
            )}
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default UpdateModpackModal;
