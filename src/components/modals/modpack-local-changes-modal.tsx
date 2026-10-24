import {
  Alert,
  AlertIcon,
  Badge,
  Box,
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
  Spinner,
  Text,
  Tooltip,
  VStack,
} from "@chakra-ui/react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { DiffFile, DiffHunks, regionHunk } from "@/components/common/diff-view";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useSharedModals } from "@/contexts/shared-modal";
import { useToast } from "@/contexts/toast";
import {
  ModpackIgnoredItem,
  ModpackLocalChange,
  ModpackLocalChangeKind,
} from "@/models/modpack-update";
import { InvokeResponse } from "@/models/response";
import { VustbAccount, isVustbManager } from "@/models/vustb";
import { AccountService } from "@/services/account";
import { ModpackUpdateService } from "@/services/modpack-update";

interface ModpackLocalChangesModalProps extends Omit<ModalProps, "children"> {
  instanceId: string;
  instanceName: string;
}

const KIND_COLORS: Record<ModpackLocalChangeKind, string> = {
  modified: "orange",
  deleted: "red",
  disabled: "gray",
  enabled: "gray",
};

/** Beyond this many files, the diffs start folded. */
const OPEN_LIMIT = 5;

/**
 * What the player changed in the managed files since the modpack installed
 * them, as a diff against the install. A manager can go on to publish.
 */
const ModpackLocalChangesModal: React.FC<ModpackLocalChangesModalProps> = ({
  instanceId,
  instanceName,
  ...modalProps
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const toast = useToast();
  const { openSharedModal } = useSharedModals();
  const { modpackChecks, checkModpackUpdate } = useGlobalData();

  const [changes, setChanges] = useState<ModpackLocalChange[] | null>(null);
  const [ignored, setIgnored] = useState<ModpackIgnoredItem[]>([]);
  const [busy, setBusy] = useState(false);
  const [vustbAccount, setVustbAccount] = useState<VustbAccount | null>(null);

  const load = useCallback(
    async (keepShown = false) => {
      if (!keepShown) setChanges(null);
      const [response, ignores] = await Promise.all([
        ModpackUpdateService.listModpackLocalChanges(instanceId),
        ModpackUpdateService.listModpackIgnores(instanceId),
      ]);
      if (response.status === "success") {
        setChanges(response.data);
      } else {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
        setChanges([]);
      }
      if (ignores.status === "success") setIgnored(ignores.data);
    },
    [instanceId, toast]
  );

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    AccountService.retrieveVustbAccount().then((response) => {
      if (response.status === "success") setVustbAccount(response.data);
    });
  }, []);

  // Keep the ledger's count in step with what is shown here.
  useEffect(() => {
    checkModpackUpdate(instanceId, true);
  }, [instanceId, checkModpackUpdate]);

  const revealFile = useCallback(
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

  /** Run an ignore or undo, then show the lists as they are now. */
  const changeIgnores = useCallback(
    async (action: () => Promise<InvokeResponse<null>>) => {
      setBusy(true);
      const response = await action();
      if (response.status !== "success") {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
      await load(true);
      // The ledger's count excludes ignored parts.
      checkModpackUpdate(instanceId, true);
      setBusy(false);
    },
    [instanceId, load, toast, checkModpackUpdate]
  );

  const isManager = isVustbManager(vustbAccount);
  // Publishing goes on top of the latest version; behind it the platform
  // refuses, so the player updates first.
  const behind = modpackChecks[instanceId]?.status === "available";
  const publishBlocker = !isManager
    ? t("ModpackLocalChangesModal.publishHint.noPermission")
    : behind
      ? t("ModpackLocalChangesModal.publishHint.updateFirst")
      : null;

  const openPublish = () => {
    modalProps.onClose();
    openSharedModal("publish-modpack", { instanceId, instanceName });
  };

  const renderBody = (change: ModpackLocalChange) => {
    if (change.kind !== "modified") {
      return (
        <Text fontSize="xs" className="secondary-text" px={3} py={2}>
          {t(`ModpackLocalChangesModal.hint.${change.kind}`)}
        </Text>
      );
    }
    if (change.hunks && change.hunks.length > 0) {
      return (
        <DiffHunks
          hunks={change.hunks}
          renderEditAction={(edit) => (
            <Button
              size="xs"
              variant="outline"
              isDisabled={busy}
              onClick={() =>
                changeIgnores(() =>
                  ModpackUpdateService.ignoreModpackLocalChange(
                    instanceId,
                    change.path,
                    change.baseline.sha1,
                    { start: edit.start, end: edit.end }
                  )
                )
              }
            >
              {t("ModpackLocalChangesModal.ignore.spot")}
            </Button>
          )}
        />
      );
    }
    return (
      <Text fontSize="xs" className="secondary-text" px={3} py={2}>
        {t(`ModpackLocalChangesModal.noDiff.${change.noDiff ?? "notText"}`)}
      </Text>
    );
  };

  return (
    <Modal
      scrollBehavior="inside"
      size={{ base: "2xl", lg: "4xl" }}
      autoFocus={false}
      {...modalProps}
    >
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>
          {t("ModpackLocalChangesModal.header.title", { name: instanceName })}
        </ModalHeader>
        <ModalCloseButton />
        <ModalBody>
          {changes === null ? (
            <VStack py={6} spacing={2}>
              <Spinner size="sm" />
              <Text fontSize="xs" className="secondary-text">
                {t("ModpackLocalChangesModal.loading")}
              </Text>
            </VStack>
          ) : changes.length === 0 ? (
            <Text className="secondary-text">
              {t("ModpackLocalChangesModal.empty")}
            </Text>
          ) : (
            <VStack align="stretch" spacing={3}>
              <Text fontSize="xs-sm">
                {t("ModpackLocalChangesModal.summary", {
                  count: changes.length,
                })}
              </Text>
              {changes.map((change) => (
                <DiffFile
                  key={change.path}
                  path={change.path}
                  hunks={change.hunks}
                  defaultOpen={changes.length <= OPEN_LIMIT}
                  tags={
                    <Badge
                      colorScheme={KIND_COLORS[change.kind]}
                      flexShrink={0}
                    >
                      {t(`ModpackLocalChangesModal.kind.${change.kind}`)}
                    </Badge>
                  }
                  actions={
                    <>
                      {change.kind !== "deleted" && (
                        <Button
                          size="xs"
                          variant="ghost"
                          onClick={() =>
                            revealFile(
                              change.kind === "disabled"
                                ? `${change.path}.disabled`
                                : change.path
                            )
                          }
                        >
                          {t("UpdateModpackModal.choice.revealFile")}
                        </Button>
                      )}
                      <Button
                        size="xs"
                        variant="ghost"
                        isDisabled={busy}
                        onClick={() =>
                          changeIgnores(() =>
                            ModpackUpdateService.ignoreModpackLocalChange(
                              instanceId,
                              change.path,
                              change.baseline.sha1
                            )
                          )
                        }
                      >
                        {t("ModpackLocalChangesModal.ignore.file")}
                      </Button>
                    </>
                  }
                >
                  {renderBody(change)}
                </DiffFile>
              ))}
            </VStack>
          )}
          {changes !== null && ignored.length > 0 && (
            <VStack align="stretch" spacing={2} mt={4}>
              <Text fontSize="xs-sm" fontWeight="medium">
                {t("ModpackLocalChangesModal.ignored.title", {
                  count: ignored.length,
                })}
              </Text>
              <Text fontSize="xs" className="secondary-text">
                {t("ModpackLocalChangesModal.ignored.description")}
              </Text>
              {ignored.map((item) => (
                <DiffFile
                  key={item.path}
                  path={item.path}
                  defaultOpen={false}
                  tags={
                    <Badge flexShrink={0}>
                      {item.whole
                        ? t("ModpackLocalChangesModal.ignored.wholeFile")
                        : t("ModpackLocalChangesModal.ignored.spots", {
                            count: item.regions.length,
                          })}
                    </Badge>
                  }
                  actions={
                    <Button
                      size="xs"
                      variant="ghost"
                      isDisabled={busy}
                      onClick={() =>
                        changeIgnores(() =>
                          ModpackUpdateService.unignoreModpackLocalChange(
                            instanceId,
                            item.path
                          )
                        )
                      }
                    >
                      {t("ModpackLocalChangesModal.ignored.undoAll")}
                    </Button>
                  }
                >
                  {!item.whole && (
                    <VStack align="stretch" spacing={0}>
                      {item.regions.map((region, index) => (
                        <Box
                          key={`${region.start}-${region.end}`}
                          borderTopWidth={index > 0 ? "1px" : 0}
                        >
                          <HStack
                            justify="space-between"
                            px={3}
                            py={1.5}
                            spacing={2}
                          >
                            <Text fontSize="xs" className="secondary-text">
                              {t("ModpackLocalChangesModal.ignored.lines", {
                                from: region.start + 1,
                                to: Math.max(region.end, region.start + 1),
                              })}
                            </Text>
                            <Button
                              size="xs"
                              variant="outline"
                              isDisabled={busy}
                              onClick={() =>
                                changeIgnores(() =>
                                  ModpackUpdateService.unignoreModpackLocalChange(
                                    instanceId,
                                    item.path,
                                    { start: region.start, end: region.end }
                                  )
                                )
                              }
                            >
                              {t("ModpackLocalChangesModal.ignored.undo")}
                            </Button>
                          </HStack>
                          <DiffHunks
                            hideHunkHeaders
                            hunks={[
                              regionHunk(
                                region.start + 1,
                                region.baseLines,
                                region.localLines
                              ),
                            ]}
                          />
                        </Box>
                      ))}
                    </VStack>
                  )}
                </DiffFile>
              ))}
            </VStack>
          )}
          {behind && isManager && changes && changes.length > 0 && (
            <Alert status="info" mt={3} fontSize="xs-sm">
              <AlertIcon />
              {t("ModpackLocalChangesModal.publishHint.updateFirst")}
            </Alert>
          )}
        </ModalBody>
        <ModalFooter>
          <Button variant="ghost" mr={2} onClick={modalProps.onClose}>
            {t("General.close")}
          </Button>
          <Tooltip label={publishBlocker} isDisabled={!publishBlocker}>
            {/* A disabled button fires no hover, so the box carries the tooltip. */}
            <Box>
              <Button
                colorScheme={primaryColor}
                isDisabled={
                  !!publishBlocker || !changes || changes.length === 0
                }
                onClick={openPublish}
              >
                {t("ModpackLocalChangesModal.button.publish")}
              </Button>
            </Box>
          </Tooltip>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default ModpackLocalChangesModal;
