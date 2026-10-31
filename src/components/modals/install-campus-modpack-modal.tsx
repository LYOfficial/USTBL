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
  VStack,
} from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { OptionItemGroup } from "@/components/common/option-item";
import { InstanceBasicSettings } from "@/components/instance-basic-settings";
import {
  gameTypesToIcon,
  loaderTypesToIcon,
} from "@/components/modals/create-instance-modal";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import { ModLoaderType } from "@/enums/instance";
import {
  CampusModpackLine,
  CampusModpackProject,
} from "@/models/campus-modpack";
import {
  ModLoaderResourceInfo,
  defaultModLoaderResourceInfo,
} from "@/models/resource";
import { CampusModpackService } from "@/services/campus-modpack";
import { InstanceService } from "@/services/instance";
import { ResourceService } from "@/services/resource";
import { runCampusInstall } from "@/utils/campus-install";
import { localizeCampusInstallError } from "@/utils/campus-install-task";
import { isDirNameInvalid, sanitizeFileName } from "@/utils/string";

interface InstallCampusModpackModalProps extends Omit<ModalProps, "children"> {
  project: CampusModpackProject;
  line: CampusModpackLine;
}

const LOADER_BY_SLUG: Record<string, ModLoaderType> = {
  vanilla: ModLoaderType.Unknown,
  fabric: ModLoaderType.Fabric,
  forge: ModLoaderType.Forge,
  legacyforge: ModLoaderType.LegacyForge,
  neoforge: ModLoaderType.NeoForge,
};

/** A long summary is folded to this many lines until it is edited. */
const SUMMARY_LINES = 3;

const InstallCampusModpackModal: React.FC<InstallCampusModpackModalProps> = ({
  project,
  line,
  ...modalProps
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const router = useRouter();
  const toast = useToast();
  const { getInstanceList } = useGlobalData();
  const primaryColor = config.appearance.theme.primaryColor;
  const { onClose } = modalProps;

  const loaderType = LOADER_BY_SLUG[line.loaderType];

  const [name, setName] = useState(sanitizeFileName(project.name));
  const [description, setDescription] = useState(project.summary);
  const [iconSrc, setIconSrc] = useState(
    loaderType && loaderType !== ModLoaderType.Unknown
      ? loaderTypesToIcon[loaderType]
      : gameTypesToIcon["release"]
  );
  const [gameDirectory, setGameDirectory] = useState(
    config.localGameDirectories[0]
  );
  const [isLoading, setIsLoading] = useState(false);

  const infoItems = useMemo(
    () => [
      {
        title: t("InstallCampusModpackModal.label.gameVersion"),
        children: line.gameVersion,
      },
      {
        title: t("InstallCampusModpackModal.label.modLoader"),
        children:
          loaderType === ModLoaderType.Unknown
            ? "-"
            : `${line.loaderType} ${line.loaderVersion}`,
      },
      {
        title: t("InstallCampusModpackModal.label.modpackVersion"),
        children: line.versionNumber,
      },
    ],
    [line, loaderType, t]
  );

  const handleInstall = useCallback(async () => {
    if (!gameDirectory || isDirNameInvalid(name) !== 0 || !loaderType) return;
    setIsLoading(true);
    try {
      const gameResp = await ResourceService.fetchGameVersionSpecific(
        line.gameVersion
      );
      if (gameResp.status !== "success") {
        toast({
          title: gameResp.message,
          description: gameResp.details,
          status: "error",
        });
        return;
      }

      let loaderInfo: ModLoaderResourceInfo = defaultModLoaderResourceInfo;
      if (loaderType !== ModLoaderType.Unknown) {
        const loaderResp = await ResourceService.fetchModLoaderVersionList(
          line.gameVersion,
          loaderType
        );
        if (loaderResp.status !== "success") {
          toast({
            title: loaderResp.message,
            description: loaderResp.details,
            status: "error",
          });
          return;
        }
        const found = loaderResp.data.find(
          (item) => item.version === line.loaderVersion
        );
        if (!found) {
          toast({
            title: t("InstallCampusModpackModal.error.loaderUnavailable", {
              loader: line.loaderType,
              version: line.loaderVersion,
            }),
            status: "error",
          });
          return;
        }
        loaderInfo = found;
      }

      // No archive: the instance is created empty with the modpack as its
      // origin, and the files arrive once the game client is ready.
      const createResp = await InstanceService.createInstance(
        gameDirectory,
        name,
        description,
        iconSrc,
        gameResp.data,
        loaderInfo,
        undefined,
        undefined,
        false,
        undefined,
        {
          platform: "campus",
          projectId: project.slug,
          versionId: "",
          expectedSha1: "",
        }
      );
      if (createResp.status === "success") {
        const registered =
          await CampusModpackService.registerCampusModpackInstall(
            `${gameDirectory.name}:${name}`,
            project.name
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
        runCampusInstall(
          { instanceId: `${gameDirectory.name}:${name}`, label: project.name },
          toast,
          t
        );
        onClose();
        router.push("/downloads");
      } else {
        toast({
          title: createResp.message,
          description: createResp.details,
          status: "error",
        });
      }
    } finally {
      setIsLoading(false);
    }
  }, [
    getInstanceList,
    description,
    gameDirectory,
    iconSrc,
    line,
    loaderType,
    name,
    onClose,
    project,
    router,
    t,
    toast,
  ]);

  return (
    <Modal
      scrollBehavior="inside"
      size={{ base: "2xl", lg: "3xl", xl: "4xl" }}
      autoFocus={false}
      {...modalProps}
    >
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>{t("InstallCampusModpackModal.header.title")}</ModalHeader>
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
              descriptionLabel={t("InstallCampusModpackModal.label.summary")}
              descriptionMaxLines={SUMMARY_LINES}
            />
            <OptionItemGroup
              title={t("InstallCampusModpackModal.label.modpackInfo")}
              items={infoItems}
              w="100%"
            />
          </VStack>
        </ModalBody>
        <ModalFooter>
          <Button
            colorScheme={primaryColor}
            onClick={handleInstall}
            isLoading={isLoading}
            isDisabled={!loaderType}
          >
            {t("InstallCampusModpackModal.button.install")}
          </Button>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default InstallCampusModpackModal;
