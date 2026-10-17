import {
  Alert,
  AlertIcon,
  Badge,
  Button,
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
  Select,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
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

type Phase = "checking" | "checked" | "planning" | "planned" | "applying";

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
  const primaryColor = config.appearance.theme.primaryColor;

  const [phase, setPhase] = useState<Phase>("checking");
  const [check, setCheck] = useState<ModpackUpdateCheck | null>(null);
  const [plan, setPlan] = useState<ModpackUpdatePlan | null>(null);
  const [choices, setChoices] = useState<Record<string, ModpackConflictChoice>>(
    {}
  );
  const [progress, setProgress] = useState<CampusInstallProgress | null>(null);
  const [confirmingRollback, setConfirmingRollback] = useState(false);

  const runCheck = useCallback(async () => {
    setPhase("checking");
    setPlan(null);
    setChoices({});
    setConfirmingRollback(false);
    // An interrupted update is rolled back before anything is compared.
    const recovered =
      await ModpackUpdateService.recoverModpackUpdate(instanceId);
    if (recovered.status === "success" && recovered.data) {
      getInstanceList(true);
    }
    // Opening the dialog is the moment to also look for local edits.
    setCheck(await checkModpackUpdate(instanceId, true));
    setPhase("checked");
  }, [instanceId, getInstanceList, checkModpackUpdate]);

  useEffect(() => {
    runCheck();
  }, [runCheck]);

  useEffect(() => {
    return CampusModpackService.onInstallProgress((payload) => {
      if (payload.instanceId === instanceId) setProgress(payload);
    });
  }, [instanceId]);

  const handlePrepare = useCallback(async () => {
    setPhase("planning");
    const response =
      await ModpackUpdateService.prepareModpackUpdate(instanceId);
    if (response.status === "success") {
      setPlan(response.data);
      setPhase("planned");
    } else {
      toast({
        title: response.message,
        description: response.details,
        status: "error",
      });
      setPhase("checked");
    }
  }, [instanceId, toast]);

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
          <VStack align="stretch" spacing={2}>
            <Text fontWeight="medium">
              {t("UpdateModpackModal.plan.conflicts", {
                count: plan.conflicts.length,
              })}
            </Text>
            {plan.conflicts.map((conflict) => (
              <HStack key={conflict.path} justify="space-between" spacing={3}>
                <VStack align="start" spacing={0} minW={0}>
                  <Text fontSize="xs-sm" noOfLines={1} wordBreak="break-all">
                    {conflict.path}
                  </Text>
                  <Text fontSize="xs" className="secondary-text">
                    {t(`UpdateModpackModal.conflictKind.${conflict.kind}`)}
                  </Text>
                </VStack>
                <Select
                  size="sm"
                  w="40%"
                  value={choices[conflict.path] ?? "keepLocal"}
                  onChange={(event) =>
                    setChoices((prev) => ({
                      ...prev,
                      [conflict.path]: event.target
                        .value as ModpackConflictChoice,
                    }))
                  }
                >
                  <option value="keepLocal">
                    {t("UpdateModpackModal.choice.keepLocal")}
                  </option>
                  <option value="takeRemote">
                    {t("UpdateModpackModal.choice.takeRemote")}
                  </option>
                </Select>
                <Button
                  size="xs"
                  variant="ghost"
                  onClick={() => revealConflictFile(conflict.path)}
                >
                  {t("UpdateModpackModal.choice.revealFile")}
                </Button>
              </HStack>
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

  return (
    <Modal
      scrollBehavior="inside"
      size={{ base: "lg", lg: "xl" }}
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
          {phase === "checked" && renderChecked()}
          {phase === "checked" && check?.canRollBack && (
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
          {phase === "planned" && renderPlan()}
          {applying && renderApplying()}
        </ModalBody>
        <ModalFooter>
          {phase === "checked" && check?.status === "available" && (
            <Button colorScheme={primaryColor} onClick={handlePrepare}>
              {t("UpdateModpackModal.button.preview")}
            </Button>
          )}
          {phase === "checked" && check?.canRollBack && (
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
