import {
  Button,
  Center,
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
import { t } from "i18next";
import { useRouter } from "next/router";
import { useCallback, useEffect, useMemo, useState } from "react";
import { BeatLoader } from "react-spinners";
import {
  OptionItemGroup,
  OptionItemGroupProps,
} from "@/components/common/option-item";
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
import { ModpackMetaInfo } from "@/models/instance/misc";
import { ModpackOptionalFile } from "@/models/modpack-update";
import { defaultModLoaderResourceInfo } from "@/models/resource";
import { CampusModpackService } from "@/services/campus-modpack";
import { InstanceService } from "@/services/instance";
import { ModpackUpdateService } from "@/services/modpack-update";
import { ResourceService } from "@/services/resource";
import { runCampusInstall } from "@/utils/campus-install";
import { localizeCampusInstallError } from "@/utils/campus-install-task";
import { isDirNameInvalid, sanitizeFileName } from "@/utils/string";

interface ImportModpackModalProps extends Omit<ModalProps, "children"> {
  path: string;
}

const ImportModpackModal: React.FC<ImportModpackModalProps> = ({
  path,
  ...modalProps
}) => {
  const { config } = useLauncherConfig();
  const { getInstanceList } = useGlobalData();
  const router = useRouter();
  const toast = useToast();
  const primaryColor = config.appearance.theme.primaryColor;
  const { onClose } = modalProps;

  const [modpack, setModpack] = useState<ModpackMetaInfo>();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [iconSrc, setIconSrc] = useState("");
  const [gameDirectory, setGameDirectory] = useState(
    config.localGameDirectories[0]
  );
  const [isPageLoading, setIsPageLoading] = useState(true);
  const [isBtnLoading, setIsBtnLoading] = useState(false);
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
    ModpackUpdateService.listLocalModpackOptionalFiles(path)
      .then((response) => {
        if (!cancelled && response.status === "success") {
          setOptionalFiles(response.data);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [path]);

  const modpackInfoGroup: OptionItemGroupProps[] = useMemo(() => {
    if (!modpack) return [];
    return [
      {
        title: t("ImportModpackModal.label.modpackInfo"),
        items: [
          {
            title: t("ImportModpackModal.label.modpackName"),
            children: modpack.name,
          },
          {
            title: t("ImportModpackModal.label.modpackVersion"),
            children: modpack.version || "-",
          },
          {
            title: t("ImportModpackModal.label.author"),
            children: modpack.author || "-",
          },
          {
            title: t("ImportModpackModal.label.modLoader"),
            children: modpack.modLoader
              ? `${modpack.modLoader.loaderType} ${modpack.modLoader.version} ${
                  modpack.modLoader.branch
                    ? `(${modpack.modLoader.branch})`
                    : ""
                }`
              : "-",
          },
          {
            title: t("ImportModpackModal.label.gameVersion"),
            children: modpack.clientVersion,
          },
        ],
      },
    ];
  }, [modpack]);

  const handleImportModpack = useCallback(async () => {
    if (!modpack || isDirNameInvalid(name) !== 0 || !gameDirectory) return;
    try {
      setIsBtnLoading(true);
      // first get client resource info
      const versionResp = await ResourceService.fetchGameVersionSpecific(
        modpack.clientVersion
      );
      if (versionResp.status !== "success") {
        toast({
          title: versionResp.message,
          description: versionResp.details,
          status: "error",
        });
        return;
      }
      const clientResourceInfo = versionResp.data;

      // The instance is created empty with the archive as its origin. The
      // update engine installs the pack once the game client is ready, like
      // any other modpack install.
      const label = modpack.version || modpack.name;
      const createResp = await InstanceService.createInstance(
        gameDirectory,
        name,
        description,
        iconSrc,
        clientResourceInfo,
        defaultModLoaderResourceInfo,
        undefined,
        false,
        undefined,
        {
          platform: "local",
          projectId: modpack.name,
          versionId: "",
          expectedSha1: "",
          label,
          loaderType: modpack.modLoader?.loaderType ?? ModLoaderType.Unknown,
          localPath: path,
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
      const instanceId = `${gameDirectory.name}:${name}`;
      const registered =
        await CampusModpackService.registerCampusModpackInstall(
          instanceId,
          label,
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
      runCampusInstall({ instanceId, label }, toast, t);
      onClose();
      router.push("/downloads");
    } catch (error) {
      logger.error("Error creating instance:", error);
    } finally {
      setIsBtnLoading(false);
    }
  }, [
    description,
    gameDirectory,
    getInstanceList,
    iconSrc,
    onClose,
    modpack,
    name,
    optionalPicks,
    path,
    router,
    toast,
  ]);

  useEffect(() => {
    setIsPageLoading(true);
    InstanceService.retrieveModpackMetaInfo(path)
      .then((response) => {
        if (response.status === "success") {
          setModpack(response.data);
          setName(sanitizeFileName(response.data.name));
          setDescription(response.data.description || "");
          setIconSrc(
            response.data.modLoader
              ? loaderTypesToIcon[response.data.modLoader.loaderType]
              : gameTypesToIcon["release"]
          );
        } else {
          toast({
            title: t("ImportModpackModal.error.invalidModpack"),
            description: response.details,
            status: "error",
          });
          onClose();
        }
      })
      .catch((error) => {
        logger.error("Error fetching modpack info:", error);
      })
      .finally(() => {
        setIsPageLoading(false);
      });
  }, [path, toast, onClose]);

  return (
    <>
      <Modal
        scrollBehavior="inside"
        size={{ base: "2xl", lg: "3xl", xl: "4xl" }}
        autoFocus={false}
        {...modalProps}
      >
        <ModalOverlay />
        <ModalContent h="100%">
          <ModalHeader>{t("ImportModpackModal.header.title")}</ModalHeader>
          <ModalCloseButton />
          <ModalBody h="100%">
            {isPageLoading ? (
              <Center h="100%">
                <BeatLoader size={16} color="gray" />
              </Center>
            ) : (
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
                />
                {modpackInfoGroup.map((group, index) => (
                  <OptionItemGroup
                    title={group.title}
                    items={group.items}
                    key={index}
                    w="100%"
                  />
                ))}
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
            )}
          </ModalBody>
          <ModalFooter>
            <Button
              colorScheme={primaryColor}
              onClick={() => handleImportModpack()}
              isLoading={isBtnLoading || isPageLoading}
            >
              {t("General.import")}
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
    </>
  );
};

export default ImportModpackModal;
