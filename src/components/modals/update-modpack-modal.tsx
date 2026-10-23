import {
  Alert,
  AlertIcon,
  Badge,
  Box,
  Button,
  ButtonGroup,
  HStack,
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
import { useRouter } from "next/router";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { DiffFile, DiffHunks, regionHunk } from "@/components/common/diff-view";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useSharedModals } from "@/contexts/shared-modal";
import { useToast } from "@/contexts/toast";
import { CampusInstallProgress } from "@/models/campus-modpack";
import {
  ModpackConflictChoice,
  ModpackUpdateCheck,
  ModpackUpdatePlan,
} from "@/models/modpack-update";
import { CampusModpackService } from "@/services/campus-modpack";
import { ModpackUpdateService } from "@/services/modpack-update";

interface UpdateModpackModalProps extends Omit<ModalProps, "children"> {
  instanceId: string;
  instanceName: string;
}

type Phase =
  | "checking"
  | "checked"
  | "planning"
  | "planned"
  | "planFailed"
  | "applying";

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
  const router = useRouter();
  const { getInstanceList, checkModpackUpdate } = useGlobalData();
  const { openGenericConfirmDialog } = useSharedModals();
  const primaryColor = config.appearance.theme.primaryColor;

  const [phase, setPhase] = useState<Phase>("checking");
  const [check, setCheck] = useState<ModpackUpdateCheck | null>(null);
  const [plan, setPlan] = useState<ModpackUpdatePlan | null>(null);
  const [choices, setChoices] = useState<Record<string, ModpackConflictChoice>>(
    {}
  );
  const [progress, setProgress] = useState<CampusInstallProgress | null>(null);
  const [confirmingRollback, setConfirmingRollback] = useState(false);
  const [planError, setPlanError] = useState<PlanError | null>(null);

  const preparePlan = useCallback(async () => {
    setPhase("planning");
    setPlanError(null);
    const response =
      await ModpackUpdateService.prepareModpackUpdate(instanceId);
    if (response.status === "success") {
      setPlan(response.data);
      setPhase("planned");
    } else {
      setPlanError({ message: response.message, details: response.details });
      setPhase("planFailed");
    }
  }, [instanceId]);

  const runCheck = useCallback(async () => {
    setPhase("checking");
    setPlan(null);
    setPlanError(null);
    setChoices({});
    setConfirmingRollback(false);
    // An interrupted update is rolled back before anything is compared.
    const recovered =
      await ModpackUpdateService.recoverModpackUpdate(instanceId);
    if (recovered.status === "success" && recovered.data) {
      getInstanceList(true);
    }
    // Opening the dialog is the moment to also look for local edits.
    const result = await checkModpackUpdate(instanceId, true);
    setCheck(result);
    // An available update goes straight to its preview.
    if (result.status === "available") {
      await preparePlan();
    } else {
      setPhase("checked");
    }
  }, [instanceId, getInstanceList, checkModpackUpdate, preparePlan]);

  useEffect(() => {
    runCheck();
  }, [runCheck]);

  useEffect(() => {
    return CampusModpackService.onInstallProgress((payload) => {
      if (payload.instanceId === instanceId) setProgress(payload);
    });
  }, [instanceId]);

  const handleApply = useCallback(async () => {
    if (!plan) return;
    setProgress(null);
    setPhase("applying");
    const response = await ModpackUpdateService.applyModpackUpdate(
      instanceId,
      plan.planHash,
      choices
    );
    getInstanceList(true);
    if (response.status === "success") {
      // The ledger reads local changes from the cached check; refresh it so
      // conflicts answered with the remote side stop counting.
      void checkModpackUpdate(instanceId, true);
      toast({
        title: response.message,
        description: plan.toLabel,
        status: "success",
      });
      if (response.data.loaderInstallPending) {
        toast({
          title: t("UpdateModpackModal.toast.loaderPending"),
          status: "info",
        });
        router.push("/downloads");
      }
      modalProps.onClose();
    } else {
      toast({
        title: response.message,
        description: response.details,
        status: "error",
      });
      // The plan may be stale or rolled back; start over from a fresh check.
      runCheck();
    }
  }, [
    plan,
    instanceId,
    choices,
    getInstanceList,
    checkModpackUpdate,
    toast,
    t,
    router,
    modalProps,
    runCheck,
  ]);

  const handleRollback = useCallback(async () => {
    setPhase("applying");
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
  }, [instanceId, getInstanceList, runCheck, toast]);

  const handleUnlink = useCallback(async () => {
    const response = await ModpackUpdateService.unlinkModpackOrigin(instanceId);
    if (response.status === "success") {
      getInstanceList(true);
      toast({ title: response.message, status: "success" });
      modalProps.onClose();
    } else {
      toast({
        title: response.message,
        description: response.details,
        status: "error",
      });
    }
  }, [instanceId, getInstanceList, toast, modalProps]);

  const confirmUnlink = useCallback(() => {
    openGenericConfirmDialog({
      title: t("UpdateModpackModal.unlink.confirmTitle"),
      body: t("UpdateModpackModal.unlink.confirmBody"),
      btnOK: t("UpdateModpackModal.unlink.button"),
      isAlert: true,
      onOKCallback: () => {
        void handleUnlink();
      },
    });
  }, [openGenericConfirmDialog, t, handleUnlink]);

  const reasonText = (code?: string) =>
    code
      ? t(`UpdateModpackModal.reason.${code}`, { defaultValue: code })
      : undefined;

  const counts = plan
    ? {
        add: plan.changes.filter((c) => c.kind === "add").length,
        update: plan.changes.filter((c) => c.kind === "update").length,
        delete: plan.changes.filter((c) => c.kind === "delete").length,
      }
    : null;

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
    if (!plan || !counts) return null;
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
        <HStack spacing={2}>
          <Badge colorScheme="green">
            {t("UpdateModpackModal.plan.add", { count: counts.add })}
          </Badge>
          <Badge colorScheme="blue">
            {t("UpdateModpackModal.plan.update", { count: counts.update })}
          </Badge>
          <Badge colorScheme="red">
            {t("UpdateModpackModal.plan.delete", { count: counts.delete })}
          </Badge>
        </HStack>
        {plan.loaderChange && (
          <Text fontSize="xs-sm">
            {t("UpdateModpackModal.plan.loaderChange", {
              from: plan.loaderChange.from.version,
              to: plan.loaderChange.to.version,
            })}
          </Text>
        )}
        {plan.autoMerged.length > 0 && (
          <Text fontSize="xs-sm" color="green.500">
            {t("UpdateModpackModal.plan.autoMerged", {
              count: plan.autoMerged.length,
            })}
          </Text>
        )}
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
        isIndeterminate={!progress || progress.total === 0}
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

  const busy = phase === "checking" || phase === "planning";
  const applying = phase === "applying";
  // Waiting for the player: the actions on the instance are open.
  const idle =
    phase === "checked" || phase === "planned" || phase === "planFailed";

  return (
    <Modal
      scrollBehavior="inside"
      // Diffs need the width; the rest of the flow stays compact.
      size={
        phase === "planned" && plan && plan.conflicts.length > 0
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
          {busy && (
            <Text className="secondary-text">
              {t(
                phase === "checking"
                  ? "UpdateModpackModal.status.checking"
                  : "UpdateModpackModal.status.planning"
              )}
            </Text>
          )}
          {(phase === "checked" || phase === "planFailed") && renderChecked()}
          {phase === "planFailed" && planError && (
            <Alert status="error" mt={3}>
              <AlertIcon />
              <VStack align="start" spacing={1}>
                <Text>{planError.message}</Text>
                {planError.details && (
                  <Text fontSize="xs">{planError.details}</Text>
                )}
              </VStack>
            </Alert>
          )}
          {phase === "planned" && check?.currentLabel && (
            <Text fontSize="xs" className="secondary-text" mb={2}>
              {t("UpdateModpackModal.label.current", {
                current: check.currentLabel,
              })}
            </Text>
          )}
          {phase === "planned" && renderPlan()}
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
          {idle &&
            check &&
            check.status !== "notFollowing" &&
            !check.unlinked && (
              <Button variant="ghost" colorScheme="red" onClick={confirmUnlink}>
                {t("UpdateModpackModal.unlink.button")}
              </Button>
            )}
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
          {phase === "checked" && check?.status === "paused" && (
            <Button colorScheme={primaryColor} onClick={handleResume}>
              {t("UpdateModpackModal.button.resume")}
            </Button>
          )}
          {phase === "checked" &&
            (check?.status === "unknown" ||
              check?.status === "needsRepair") && (
              <Button variant="ghost" onClick={runCheck}>
                {t("UpdateModpackModal.button.retry")}
              </Button>
            )}
          {phase === "planFailed" && (
            <Button colorScheme={primaryColor} onClick={preparePlan}>
              {t("UpdateModpackModal.button.retry")}
            </Button>
          )}
          {phase === "planned" && plan && !plan.reinstallRequired && (
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
