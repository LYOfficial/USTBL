import {
  Alert,
  AlertIcon,
  Button,
  HStack,
  Icon,
  IconButton,
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
  Tooltip,
  VStack,
} from "@chakra-ui/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { LuFoldVertical, LuUnfoldVertical } from "react-icons/lu";
import { DiffExpandSignal } from "@/components/common/diff-view";
import FileFilterInput from "@/components/common/file-filter-input";
import {
  ConflictItem,
  PlanChangeGroups,
} from "@/components/modpack-update-lists";
import { VersionChangeRow } from "@/components/modpack-version-change";
import { OptionalFilesList } from "@/components/optional-files-list";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import { InstanceError } from "@/enums/service-error";
import { CampusInstallProgress } from "@/models/campus-modpack";
import {
  ModpackConflictChoice,
  ModpackUpdateCheck,
  ModpackUpdatePlan,
} from "@/models/modpack-update";
import { CampusModpackService } from "@/services/campus-modpack";
import { ModpackUpdateService } from "@/services/modpack-update";
import { createPathMatcher, isQueryActive } from "@/utils/fuzzy-filter";
import { componentDisplayName } from "@/utils/modpack-components";
import {
  failedUpdatePlans,
  rememberedChoices,
  resolvePlanChoices,
  withDefaultChoices,
} from "@/utils/modpack-update-choices";
import { sortConflicts } from "@/utils/modpack-update-order";
import { pendingUpdateLabels } from "@/utils/rename-suggestion";

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

  // A file picked to update a local import is only kept while this dialog is
  // open. An applied update clears it itself, so this is a no-op then.
  const handleClose = useCallback(() => {
    const platform = getInstanceList()?.find(
      (instance) => instance.id === instanceId
    )?.origin?.platform;
    if (platform === "local") {
      void ModpackUpdateService.discardLocalModpackUpdate(instanceId);
    }
    modalProps.onClose();
  }, [getInstanceList, instanceId, modalProps]);

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
  const [fileQuery, setFileQuery] = useState("");
  const choicesRef = useRef(choices);
  choicesRef.current = choices;
  // Picks among the optional files this version added. They are part of the
  // plan: the backend hashes them, so a change prepares the plan again.
  const [optionalPicks, setOptionalPicks] = useState<string[]>([]);
  const [replanning, setReplanning] = useState(false);
  const optionalPicksRef = useRef(optionalPicks);
  optionalPicksRef.current = optionalPicks;
  const plannedPicksRef = useRef<string[]>([]);
  const replanInFlight = useRef(false);
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
        const response = await ModpackUpdateService.prepareModpackUpdate(
          instanceId,
          optionalPicksRef.current
        );
        if (roundRef.current !== round) return;
        if (response.status === "success") {
          plannedPicksRef.current = optionalPicksRef.current;
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
          response.raw_error !== InstanceError.ModpackUpdatePrepareCancelled
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

  const replanForPicks = useCallback(async () => {
    if (replanInFlight.current) return;
    replanInFlight.current = true;
    setReplanning(true);
    try {
      let asked: string[];
      do {
        asked = optionalPicksRef.current;
        const response = await ModpackUpdateService.prepareModpackUpdate(
          instanceId,
          asked
        );
        if (response.status === "success") {
          plannedPicksRef.current = asked;
          setPlan(response.data);
        } else {
          toast({
            title: response.message,
            description: response.details,
            status: "error",
          });
          optionalPicksRef.current = plannedPicksRef.current;
          setOptionalPicks(plannedPicksRef.current);
          break;
        }
      } while (optionalPicksRef.current !== asked);
    } finally {
      replanInFlight.current = false;
      setReplanning(false);
    }
  }, [instanceId, toast]);

  const handlePicksChange = useCallback(
    (picks: string[]) => {
      optionalPicksRef.current = picks;
      setOptionalPicks(picks);
      void replanForPicks();
    },
    [replanForPicks]
  );

  const handleApply = useCallback(async () => {
    if (!plan || applyInFlight.current) return;
    applyInFlight.current = true;
    try {
      rememberedChoices.set(instanceId, { planHash: plan.planHash, choices });
      setProgress(null);
      setCheckPhase("applying");
      // Take the plan off the screen so its buttons stop rendering.
      setPlanPhase("idle");
      // Note the old and new version labels for the rename suggestion once
      // the update finishes; without an old label nothing is suggested.
      const fromLabel = getInstanceList()?.find(
        (instance) => instance.id === instanceId
      )?.modpackVersion;
      if (fromLabel && plan.toLabel) {
        pendingUpdateLabels.set(instanceId, {
          from: fromLabel,
          to: plan.toLabel,
        });
      } else {
        pendingUpdateLabels.delete(instanceId);
      }
      const response = await ModpackUpdateService.applyModpackUpdate(
        instanceId,
        plan.planHash,
        withDefaultChoices(plan.conflicts, choices),
        optionalPicksRef.current
      );
      if (response.status !== "success") pendingUpdateLabels.delete(instanceId);
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
  }, [
    plan,
    instanceId,
    choices,
    instanceName,
    getInstanceList,
    toast,
    t,
    modalProps,
    runCheck,
  ]);

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
        if (conflict.hunks.length === 0) {
          next[conflict.path] = choice;
        } else {
          conflict.hunks.forEach((_, index) => {
            next[`${conflict.path}#${index}`] = choice;
          });
        }
      }
      setChoices(next);
    },
    [plan]
  );

  // Display only: answers and "all" buttons still cover every conflict.
  const matchesPath = useMemo(() => createPathMatcher(fileQuery), [fileQuery]);
  const shownPlan = useMemo(() => {
    if (!plan || !isQueryActive(fileQuery)) return plan;
    return {
      ...plan,
      changes: plan.changes.filter((change) => matchesPath(change.path)),
      autoMerged: plan.autoMerged.filter((change) => matchesPath(change.path)),
    };
  }, [plan, fileQuery, matchesPath]);
  // Display order only: `choices` and the plan hash do not depend on it.
  const shownConflicts = useMemo(
    () =>
      plan
        ? sortConflicts(plan.conflicts).filter((conflict) =>
            matchesPath(conflict.path)
          )
        : [],
    [plan, matchesPath]
  );

  const chooseOne = useCallback(
    (key: string, choice: ModpackConflictChoice) =>
      setChoices((prev) => ({ ...prev, [key]: choice })),
    []
  );

  /** Answer all blocks of one file with a single update. */
  const chooseMany = useCallback(
    (keys: string[], choice: ModpackConflictChoice) =>
      setChoices((prev) => {
        const next = { ...prev };
        for (const key of keys) next[key] = choice;
        return next;
      }),
    []
  );

  // Conflict cards start open; the two buttons open or close every card at once.
  const [expandSignal, setExpandSignal] = useState<DiffExpandSignal>({
    open: true,
    token: 0,
  });
  const expandAll = useCallback(
    (open: boolean) =>
      setExpandSignal((prev) => ({ open, token: prev.token + 1 })),
    []
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
    return (
      <VStack align="stretch" spacing={3}>
        <Text>
          {t("UpdateModpackModal.plan.summary", { latest: plan.toLabel })}
        </Text>
        {!!plan.newOptional?.length && (
          <VStack align="stretch" spacing={2}>
            <Text fontWeight="bold">{t("OptionalFiles.newTitle")}</Text>
            <Text fontSize="xs-sm" className="secondary-text">
              {t("OptionalFiles.newDescription")}
            </Text>
            <OptionalFilesList
              files={plan.newOptional}
              selected={optionalPicks}
              onChange={handlePicksChange}
              isDisabled={replanning}
            />
          </VStack>
        )}
        {(!!plan.componentChanges?.length || plan.loaderChange) && (
          <VStack align="stretch" spacing={1}>
            <Text fontWeight="medium">
              {t("UpdateModpackModal.plan.componentChanges.title")}
            </Text>
            {plan.componentChanges?.length ? (
              plan.componentChanges.map((change) => (
                <VersionChangeRow
                  key={change.uid}
                  name={componentDisplayName(change.uid)}
                  from={change.from}
                  to={change.to}
                />
              ))
            ) : plan.loaderChange ? (
              <VersionChangeRow
                name={plan.loaderChange.to.type}
                from={plan.loaderChange.from.version}
                to={plan.loaderChange.to.version}
              />
            ) : null}
          </VStack>
        )}
        {plan.restoreComponents && (
          <Text fontSize="xs-sm">
            {t("UpdateModpackModal.plan.restoreComponents")}
          </Text>
        )}
        {plan.changes.length + plan.autoMerged.length + plan.conflicts.length >
          0 && <FileFilterInput value={fileQuery} onChange={setFileQuery} />}
        {shownPlan &&
          isQueryActive(fileQuery) &&
          shownPlan.changes.length +
            shownPlan.autoMerged.length +
            shownConflicts.length ===
            0 && (
            <Text fontSize="xs-sm" className="secondary-text">
              {t("FileFilter.empty")}
            </Text>
          )}
        <PlanChangeGroups instanceId={instanceId} plan={shownPlan ?? plan} />
        {plan.conflicts.length > 0 && (
          <VStack align="stretch" spacing={3}>
            <HStack justify="space-between" spacing={2}>
              <HStack spacing={2} minW={0}>
                <Text fontWeight="medium">
                  {t("UpdateModpackModal.plan.conflicts", {
                    count: plan.conflicts.length,
                  })}
                </Text>
                {isQueryActive(fileQuery) && (
                  <Text fontSize="xs" className="secondary-text">
                    {t("FileFilter.shown", {
                      shown: shownConflicts.length,
                      total: plan.conflicts.length,
                    })}
                  </Text>
                )}
              </HStack>
              <HStack spacing={1} flexShrink={0}>
                {(
                  [
                    [true, "expandAll", LuUnfoldVertical],
                    [false, "collapseAll", LuFoldVertical],
                  ] as const
                ).map(([open, key, icon]) => (
                  <Tooltip
                    key={key}
                    label={t(`UpdateModpackModal.choice.${key}`)}
                  >
                    <IconButton
                      size="xs"
                      variant="ghost"
                      aria-label={t(`UpdateModpackModal.choice.${key}`)}
                      icon={<Icon as={icon} />}
                      onClick={() => expandAll(open)}
                    />
                  </Tooltip>
                ))}
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
            {shownConflicts.map((conflict) => (
              <ConflictItem
                key={conflict.path}
                conflict={conflict}
                choices={choices}
                primaryColor={primaryColor}
                onChoose={chooseOne}
                onChooseMany={chooseMany}
                onReveal={revealConflictFile}
                expandSignal={expandSignal}
              />
            ))}
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
      onClose={handleClose}
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
          {!checking && planPhase === "planFailed" && planError && (
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
              <Button
                colorScheme={primaryColor}
                onClick={handleApply}
                isDisabled={replanning}
              >
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
