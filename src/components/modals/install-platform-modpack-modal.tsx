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
  useDisclosure,
} from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { OptionItemGroup } from "@/components/common/option-item";
import { InstanceBasicSettings } from "@/components/instance-basic-settings";
import {
  gameTypesToIcon,
  loaderTypesToIcon,
} from "@/components/modals/create-instance-modal";
import { OptionalFilesList } from "@/components/optional-files-list";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import { ModLoaderType } from "@/enums/instance";
import { OtherResourceSource } from "@/enums/resource";
import { useFolderConflict } from "@/hooks/folder-conflict";
import { ModpackOptionalFile } from "@/models/modpack-update";
import { OtherResourceFileInfo } from "@/models/resource";
import { CampusModpackService } from "@/services/campus-modpack";
import { InstanceService } from "@/services/instance";
import { ModpackUpdateService } from "@/services/modpack-update";
import { ResourceService } from "@/services/resource";
import { runCampusInstall } from "@/utils/campus-install";
import { localizeCampusInstallError } from "@/utils/campus-install-task";
import { isDirNameInvalid, sanitizeFileName } from "@/utils/string";

interface InstallPlatformModpackModalProps extends Omit<
  ModalProps,
  "children"
> {
  projectName: string;
  projectDescription: string;
  projectIconSrc: string;
  projectId: string;
  source: OtherResourceSource;
  file: OtherResourceFileInfo;
  gameVersion: string;
}

const LOADER_TYPES: Record<string, ModLoaderType> = {
  fabric: ModLoaderType.Fabric,
  forge: ModLoaderType.Forge,
  legacyforge: ModLoaderType.LegacyForge,
  neoforge: ModLoaderType.NeoForge,
  quilt: ModLoaderType.Quilt,
  liteloader: ModLoaderType.LiteLoader,
};

const loaderTypeOf = (loader?: string): ModLoaderType =>
  LOADER_TYPES[loader?.toLowerCase() ?? ""] ?? ModLoaderType.Unknown;

const InstallPlatformModpackModal: React.FC<
  InstallPlatformModpackModalProps
> = ({
  projectName,
  projectDescription,
  projectIconSrc,
  projectId,
  source,
  file,
  gameVersion,
  ...modalProps
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const { getInstanceList } = useGlobalData();
  const { resolveFolder, folderConflictDialog } = useFolderConflict();
  const router = useRouter();
  const toast = useToast();
  const primaryColor = config.appearance.theme.primaryColor;
  const { onClose } = modalProps;
  const loaderType = loaderTypeOf(file.loader);

  const [name, setName] = useState(sanitizeFileName(projectName));
  const [description, setDescription] = useState(projectDescription);
  const [iconSrc, setIconSrc] = useState(
    loaderType === ModLoaderType.Unknown
      ? projectIconSrc || gameTypesToIcon.release
      : loaderTypesToIcon[loaderType]
  );
  const [gameDirectory, setGameDirectory] = useState(
    config.localGameDirectories[0]
  );
  const [isLoading, setIsLoading] = useState(false);
  const [optionalFiles, setOptionalFiles] = useState<ModpackOptionalFile[]>([]);
  const [optionalPicks, setOptionalPicks] = useState<string[]>([]);
  const {
    isOpen: isOptionalOpen,
    onOpen: onOptionalOpen,
    onClose: onOptionalClose,
  } = useDisclosure();

  // The list is a convenience: when it cannot be read the pack installs with
  // no optional file, the default.
  useEffect(() => {
    let cancelled = false;
    setOptionalFiles([]);
    setOptionalPicks([]);
    if (!file.downloadUrl || !file.sha1) return;
    ModpackUpdateService.listModpackOptionalFiles(file.downloadUrl, file.sha1)
      .then((response) => {
        if (!cancelled && response.status === "success") {
          setOptionalFiles(response.data);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [file.downloadUrl, file.sha1]);

  const infoItems = useMemo(
    () => [
      {
        title: t("InstallPlatformModpackModal.label.gameVersion"),
        children: gameVersion,
      },
      {
        title: t("InstallPlatformModpackModal.label.modLoader"),
        children:
          file.loader || t("InstallPlatformModpackModal.label.manifest"),
      },
      {
        title: t("InstallPlatformModpackModal.label.modpackVersion"),
        children: file.name,
      },
    ],
    [file, gameVersion, t]
  );

  const handleInstall = useCallback(async () => {
    if (!gameDirectory || isDirNameInvalid(name) !== 0) return;
    setIsLoading(true);
    try {
      const gameResp =
        await ResourceService.fetchGameVersionSpecific(gameVersion);
      if (gameResp.status !== "success") {
        toast({
          title: gameResp.message,
          description: gameResp.details,
          status: "error",
        });
        return;
      }
      const folder = await resolveFolder(gameDirectory, name);
      if (folder === null) return;
      const createResp = await InstanceService.createInstance(
        gameDirectory,
        folder,
        description,
        iconSrc,
        gameResp.data,
        {
          loaderType: ModLoaderType.Unknown,
          version: "",
          description: "",
          stable: true,
        },
        undefined,
        false,
        undefined,
        {
          platform:
            source === OtherResourceSource.CurseForge
              ? "curseforge"
              : "modrinth",
          projectId,
          versionId: file.fileId,
          expectedSha1: file.sha1,
          url: file.downloadUrl,
          label: file.name,
          loaderType,
        }
      );
      if (createResp.status !== "success") {
        toast({
          title: createResp.message,
          description: createResp.details,
          status: "error",
        });
        return;
      }
      const instanceId = createResp.data;
      const registered =
        await CampusModpackService.registerCampusModpackInstall(
          instanceId,
          file.name,
          optionalPicks
        );
      if (registered.status !== "success") {
        toast({
          title: t("Services.instance.installCampusModpack.error.title"),
          description: localizeCampusInstallError(
            t,
            String(registered.raw_error ?? registered.details ?? "")
          ),
          status: "error",
        });
        return;
      }
      getInstanceList(true);
      runCampusInstall({ instanceId, label: file.name }, toast, t);
      onClose();
      router.push("/downloads");
    } finally {
      setIsLoading(false);
    }
  }, [
    description,
    file,
    gameDirectory,
    gameVersion,
    getInstanceList,
    iconSrc,
    loaderType,
    name,
    onClose,
    optionalPicks,
    projectId,
    resolveFolder,
    router,
    source,
    t,
    toast,
  ]);

  return (
    <>
      <Modal
        scrollBehavior="inside"
        size={{ base: "2xl", lg: "3xl", xl: "4xl" }}
        autoFocus={false}
        {...modalProps}
      >
        <ModalOverlay />
        <ModalContent>
          <ModalHeader>
            {t("InstallPlatformModpackModal.header.title")}
          </ModalHeader>
          <ModalCloseButton />
          <ModalBody>
            <VStack w="100%" spacing={4}>
              <InstanceBasicSettings
                name={name}
                setName={setName}
                description={description}
                setDescription={setDescription}
                iconSrc={iconSrc}
                setIconSrc={setIconSrc}
                gameDirectory={gameDirectory}
                setGameDirectory={setGameDirectory}
                descriptionLabel={t(
                  "InstallPlatformModpackModal.label.summary"
                )}
                descriptionMaxLines={3}
              />
              <OptionItemGroup
                title={t("InstallPlatformModpackModal.label.modpackInfo")}
                items={infoItems}
                w="100%"
              />
              {optionalFiles.length > 0 && (
                <Button
                  variant="outline"
                  alignSelf="flex-start"
                  onClick={onOptionalOpen}
                >
                  {t("OptionalFiles.entry", {
                    selected: optionalPicks.length,
                    total: optionalFiles.length,
                  })}
                </Button>
              )}
            </VStack>
          </ModalBody>
          <ModalFooter>
            <Button
              colorScheme={primaryColor}
              onClick={handleInstall}
              isLoading={isLoading}
            >
              {t("InstallPlatformModpackModal.button.install")}
            </Button>
          </ModalFooter>
        </ModalContent>
      </Modal>
      <Modal
        isOpen={isOptionalOpen}
        onClose={onOptionalClose}
        scrollBehavior="inside"
        autoFocus={false}
      >
        <ModalOverlay />
        <ModalContent>
          <ModalHeader>{t("OptionalFiles.title")}</ModalHeader>
          <ModalCloseButton />
          <ModalBody>
            <VStack align="stretch" spacing={3}>
              <Text fontSize="xs-sm" className="secondary-text">
                {t("OptionalFiles.description")}
              </Text>
              <OptionalFilesList
                files={optionalFiles}
                selected={optionalPicks}
                onChange={setOptionalPicks}
              />
            </VStack>
          </ModalBody>
        </ModalContent>
      </Modal>
      {folderConflictDialog}
    </>
  );
};

export default InstallPlatformModpackModal;
