import {
  Button,
  Modal,
  ModalBody,
  ModalCloseButton,
  ModalContent,
  ModalFooter,
  ModalHeader,
  ModalOverlay,
  ModalProps,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useSharedModals } from "@/contexts/shared-modal";
import { useToast } from "@/contexts/toast";
import { CampusModpackProject } from "@/models/campus-modpack";
import { CampusModpackService } from "@/services/campus-modpack";
import { ModpackUpdateService } from "@/services/modpack-update";
import { campusProjectRefs, loaderSlug } from "@/utils/modpack-ledger";

interface ModpackInstanceDetailsModalProps extends Omit<
  ModalProps,
  "children"
> {
  instanceId: string;
  instanceName: string;
}

/**
 * Details of one followed instance: its identity, the line it follows, and
 * the follow actions (pause/resume, unlink, view mods).
 */
const ModpackInstanceDetailsModal: React.FC<
  ModpackInstanceDetailsModalProps
> = ({ instanceId, instanceName, ...modalProps }) => {
  const { t } = useTranslation();
  const router = useRouter();
  const toast = useToast();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const { getInstanceList, checkModpackUpdate, modpackChecks } =
    useGlobalData();
  const { openGenericConfirmDialog } = useSharedModals();

  const instances = useMemo(() => getInstanceList() ?? [], [getInstanceList]);
  const instance = useMemo(
    () => instances.find((i) => i.id === instanceId),
    [instances, instanceId]
  );
  const check = modpackChecks[instanceId];

  const [projects, setProjects] = useState<CampusModpackProject[]>([]);

  useEffect(() => {
    if (instance?.origin?.platform === "campus") {
      CampusModpackService.listCampusModpackProjects().then((response) => {
        if (response.status === "success") setProjects(response.data);
      });
    }
  }, [instance?.origin?.platform]);

  const origin = instance?.origin;
  const project =
    origin?.platform === "campus"
      ? projects.find((p) => campusProjectRefs(p).includes(origin.projectId))
      : undefined;

  const lineLabel = origin
    ? [
        origin.platform === "campus"
          ? t("InstanceSyncPage.ledger.platform.campus")
          : origin.platform === "modrinth"
            ? "Modrinth"
            : "CurseForge",
        origin.gameVersion,
        loaderSlug(origin.loaderType) === "vanilla"
          ? t("InstanceSyncPage.platform.vanilla")
          : origin.loaderType,
      ].join(" · ")
    : "";

  const projectLabel = useMemo(() => {
    if (!origin) return null;
    if (origin.platform !== "campus") return null;
    if (project) return project.name;
    return t("ModpackInstanceDetailsModal.follow.projectGone");
  }, [origin, project, t]);

  const latestLabel = check?.status === "available" ? check.latestLabel : null;

  const handleSetFollow = useCallback(
    async (follow: boolean) => {
      const response = await ModpackUpdateService.setModpackFollow(
        instanceId,
        follow
      );
      if (response.status === "success") {
        getInstanceList(true);
        void checkModpackUpdate(instanceId, true);
      } else {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
    },
    [instanceId, getInstanceList, checkModpackUpdate, toast]
  );

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

  const openMods = useCallback(() => {
    modalProps.onClose();
    router.push(`/instances/details/${encodeURIComponent(instanceId)}/mods`);
  }, [modalProps, router, instanceId]);

  if (!instance || !origin) {
    return (
      <Modal size="lg" {...modalProps}>
        <ModalOverlay />
        <ModalContent>
          <ModalHeader>
            {t("ModpackInstanceDetailsModal.header.title", {
              name: instanceName,
            })}
          </ModalHeader>
          <ModalCloseButton />
          <ModalBody>
            <Text className="secondary-text">
              {t("ModpackInstanceDetailsModal.notFollowing")}
            </Text>
          </ModalBody>
        </ModalContent>
      </Modal>
    );
  }

  return (
    <Modal size="lg" {...modalProps}>
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>
          {t("ModpackInstanceDetailsModal.header.title", {
            name: instanceName,
          })}
        </ModalHeader>
        <ModalCloseButton />
        <ModalBody>
          <VStack align="stretch" spacing={4}>
            <VStack align="stretch" spacing={1}>
              <Text fontSize="sm" fontWeight="bold">
                {t("ModpackInstanceDetailsModal.section.instance")}
              </Text>
              <Text fontSize="xs-sm">
                {t("ModpackInstanceDetailsModal.instance.name")}:{" "}
                {instance.name}
              </Text>
              <Text fontSize="xs-sm">
                {t("ModpackInstanceDetailsModal.instance.version")}:{" "}
                {instance.version}
              </Text>
              <Text fontSize="xs-sm">
                {t("ModpackInstanceDetailsModal.instance.loader")}:{" "}
                {instance.modLoader.loaderType}{" "}
                {instance.modLoader.version ?? ""}
              </Text>
              {instance.modpackVersion && (
                <Text fontSize="xs-sm">
                  {t("ModpackInstanceDetailsModal.instance.modpackVersion")}:{" "}
                  {instance.modpackVersion}
                </Text>
              )}
            </VStack>

            <VStack align="stretch" spacing={1}>
              <Text fontSize="sm" fontWeight="bold">
                {t("ModpackInstanceDetailsModal.section.follow")}
              </Text>
              <Text fontSize="xs-sm">
                {t("ModpackInstanceDetailsModal.follow.platform")}: {lineLabel}
              </Text>
              {projectLabel && (
                <Text fontSize="xs-sm">
                  {t("ModpackInstanceDetailsModal.follow.project")}:{" "}
                  {projectLabel}
                </Text>
              )}
              <Text fontSize="xs-sm">
                {t("ModpackInstanceDetailsModal.follow.current")}:{" "}
                {check?.currentLabel ?? instance.modpackVersion ?? "-"}
              </Text>
              {latestLabel && (
                <Text fontSize="xs-sm">
                  {t("ModpackInstanceDetailsModal.follow.latest")}:{" "}
                  {latestLabel}
                </Text>
              )}
              <Text fontSize="xs-sm">
                {t("ModpackInstanceDetailsModal.follow.status")}:{" "}
                {origin.follow
                  ? t("ModpackInstanceDetailsModal.follow.following")
                  : t("ModpackInstanceDetailsModal.follow.paused")}
              </Text>
            </VStack>
          </VStack>
        </ModalBody>
        <ModalFooter gap={2}>
          <Button variant="ghost" colorScheme="red" onClick={confirmUnlink}>
            {t("ModpackInstanceDetailsModal.action.unlink")}
          </Button>
          <Button variant="outline" onClick={openMods}>
            {t("ModpackInstanceDetailsModal.action.viewMods")}
          </Button>
          <Button
            colorScheme={primaryColor}
            onClick={() => handleSetFollow(!origin.follow)}
          >
            {origin.follow
              ? t("ModpackInstanceDetailsModal.action.pause")
              : t("ModpackInstanceDetailsModal.action.resume")}
          </Button>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default ModpackInstanceDetailsModal;
