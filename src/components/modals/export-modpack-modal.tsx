import {
  Button,
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
  NumberInput,
  NumberInputField,
  Switch,
  Text,
  VStack,
} from "@chakra-ui/react";
import { save } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { FaInfinity } from "react-icons/fa6";
import { SiModrinth } from "react-icons/si";
import Editable from "@/components/common/editable";
import ModpackFileTree from "@/components/common/modpack-file-tree";
import {
  OptionItemGroup,
  OptionItemGroupProps,
} from "@/components/common/option-item";
import SegmentedControl from "@/components/common/segmented";
import { useLauncherConfig } from "@/contexts/config";
import { useToast } from "@/contexts/toast";
import { ExportModpackFormat } from "@/enums/instance";
import { ExportModpackOptions, ModpackFileList } from "@/models/instance/misc";
import { InstanceService } from "@/services/instance";
import { isFileNameSanitized, sanitizeFileName } from "@/utils/string";

interface ExportModpackModalProps extends Omit<ModalProps, "children"> {
  instanceId: string;
  instanceName: string;
  isComponentProfile?: boolean;
}

const ExportModpackModal: React.FC<ExportModpackModalProps> = ({
  instanceId,
  instanceName,
  isComponentProfile = false,
  ...modalProps
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const toast = useToast();
  const primaryColor = config.appearance.theme.primaryColor;

  // A component profile cannot be expressed as a Modrinth manifest.
  const [exportFormat, setExportFormat] = useState<ExportModpackFormat>(
    isComponentProfile
      ? ExportModpackFormat.MultiMC
      : ExportModpackFormat.Modrinth
  );
  const [modpackName, setModpackName] = useState(instanceName);
  const [modpackVersion, setModpackVersion] = useState("1.0.0");
  const [author, setAuthor] = useState("");
  const [description, setDescription] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [fileList, setFileList] = useState<ModpackFileList | null>(null);
  const [selectedFiles, setSelectedFiles] = useState<Set<string>>(new Set());
  const [isFileListLoading, setIsFileListLoading] = useState(false);
  const [packWithLauncher, setPackWithLauncher] = useState(false);
  const [minMemoryInput, setMinMemoryInput] = useState("");
  const [noCreateRemoteFiles, setNoCreateRemoteFiles] = useState(false);

  const checkNameError = useCallback((value: string): number => {
    if (value.trim() === "") return 1;
    if (!isFileNameSanitized(value)) return 2;
    if (value.length > 255) return 3;
    return 0;
  }, []);

  const checkVersionError = useCallback((value: string): number => {
    if (value.trim() === "") return 1;
    return 0;
  }, []);

  useEffect(() => {
    if (!modalProps.isOpen) return;
    let isActive = true;
    setIsFileListLoading(true);

    InstanceService.retrieveExportableFileList(instanceId)
      .then((response) => {
        if (!isActive) return;
        if (response.status === "success") {
          setFileList(response.data);
          const nextSelected = new Set(response.data.all);
          response.data.unchecked.forEach((path) => nextSelected.delete(path));
          setSelectedFiles(nextSelected);
        } else {
          toast({
            title: response.message,
            description: response.details,
            status: "error",
          });
        }
      })
      .catch((error) => {
        if (!isActive) return;
        toast({
          title: t("Services.instance.retrieveExportableFileList.error.title"),
          description: String(error),
          status: "error",
        });
      })
      .finally(() => {
        if (!isActive) return;
        setIsFileListLoading(false);
      });

    return () => {
      isActive = false;
    };
  }, [instanceId, modalProps.isOpen, t, toast]);

  const selectedFileList = useMemo(() => {
    if (!fileList) return [];
    return fileList.all.filter((path) => selectedFiles.has(path));
  }, [fileList, selectedFiles]);

  const isMultiMC = exportFormat === "MultiMC";
  const isModrinth = exportFormat === "Modrinth";

  const handleExport = useCallback(async () => {
    // Validate inputs
    if (checkNameError(modpackName) !== 0) {
      toast({
        title: t("ExportModpackModal.error.invalidName"),
        status: "warning",
      });
      return;
    }

    if (checkVersionError(modpackVersion) !== 0) {
      toast({
        title: t("ExportModpackModal.error.invalidVersion"),
        status: "warning",
      });
      return;
    }

    if (selectedFileList.length === 0) {
      toast({
        title: t("ExportModpackModal.error.noFileSelected"),
        status: "warning",
      });
      return;
    }

    // Open save dialog
    const fileExtension = exportFormat === "Modrinth" ? "mrpack" : "zip";
    const savePath = await save({
      defaultPath: `${sanitizeFileName(modpackName)}-${modpackVersion}.${fileExtension}`,
      filters: [
        {
          name: t("General.dialog.filterName.modpack"),
          extensions: [fileExtension],
        },
      ],
    });

    if (!savePath) {
      return; // User cancelled
    }

    setIsLoading(true);

    try {
      const normalizedMinMemoryInput = minMemoryInput.trim();
      const parsedMinMemory = /^\d+$/.test(normalizedMinMemoryInput)
        ? Number.parseInt(normalizedMinMemoryInput, 10)
        : undefined;

      const options: ExportModpackOptions = {
        format: exportFormat,
        name: modpackName,
        version: modpackVersion,
        author: author || undefined,
        description: description || undefined,
        packWithLauncher: packWithLauncher || undefined,
        minMemory: parsedMinMemory,
        noCreateRemoteFiles: noCreateRemoteFiles || undefined,
      };

      const response = await InstanceService.exportModpack(
        instanceId,
        savePath,
        options,
        selectedFileList
      );

      if (response.status === "success") {
        toast({
          title: response.message,
          status: "success",
        });
        modalProps.onClose?.();
      } else {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
    } catch (error) {
      toast({
        title: t("Services.instance.exportModpack.error.title"),
        description: String(error),
        status: "error",
      });
    } finally {
      setIsLoading(false);
    }
  }, [
    checkNameError,
    modpackName,
    checkVersionError,
    modpackVersion,
    exportFormat,
    t,
    toast,
    minMemoryInput,
    author,
    description,
    packWithLauncher,
    noCreateRemoteFiles,
    instanceId,
    selectedFileList,
    modalProps,
  ]);

  const optionGroups: OptionItemGroupProps[] = [
    {
      items: [
        {
          title: t("ExportModpackModal.label.exportFormat"),
          children: (
            <SegmentedControl
              selected={exportFormat}
              onSelectItem={(val) =>
                setExportFormat(val as ExportModpackFormat)
              }
              size="xs"
              items={(isComponentProfile
                ? (["MultiMC"] as const)
                : (["Modrinth", "MultiMC"] as const)
              ).map((format) => ({
                value: format,
                tooltip: t(
                  `ExportModpackModal.tooltip.exportFormat.${
                    isComponentProfile ? "MultiMCComponentOnly" : format
                  }`
                ),
                label: (
                  <HStack spacing={2}>
                    <Icon
                      as={format === "Modrinth" ? SiModrinth : FaInfinity}
                    />
                    <Text>{format}</Text>
                  </HStack>
                ),
              }))}
              withTooltip
            />
          ),
        },
      ],
    },
    {
      title: t("ExportModpackModal.label.basicInfo"),
      items: [
        {
          title: t("ExportModpackModal.label.modpackName"),
          children: (
            <Editable
              isTextArea={false}
              value={modpackName}
              onEditSubmit={setModpackName}
              textProps={{ className: "secondary-text", fontSize: "xs-sm" }}
              inputProps={{ fontSize: "xs-sm" }}
              formErrMsgProps={{ fontSize: "xs-sm" }}
              checkError={checkNameError}
              localeKey="ExportModpackModal.errorMessage"
            />
          ),
        },
        {
          title: t("ExportModpackModal.label.modpackVersion"),
          children: (
            <Editable
              isTextArea={false}
              value={modpackVersion}
              onEditSubmit={setModpackVersion}
              textProps={{ className: "secondary-text", fontSize: "xs-sm" }}
              inputProps={{ fontSize: "xs-sm" }}
              formErrMsgProps={{ fontSize: "xs-sm" }}
              checkError={checkVersionError}
              localeKey="ExportModpackModal.errorMessage"
            />
          ),
        },
        {
          title: t("ExportModpackModal.label.author"),
          children: (
            <Editable
              isTextArea={false}
              value={author}
              onEditSubmit={setAuthor}
              textProps={{ className: "secondary-text", fontSize: "xs-sm" }}
              inputProps={{
                fontSize: "xs-sm",
                placeholder: t("General.optional"),
              }}
            />
          ),
        },
        {
          title: t("ExportModpackModal.label.description"),
          children: (
            <Editable
              isTextArea={true}
              value={description}
              onEditSubmit={setDescription}
              textProps={{ className: "secondary-text", fontSize: "xs-sm" }}
              inputProps={{
                fontSize: "xs-sm",
                placeholder: t("General.optional"),
              }}
            />
          ),
        },
      ],
    },
    {
      title: t("ExportModpackModal.label.includedFiles"),
      items: [
        <ModpackFileTree
          key="included-files-tree"
          paths={fileList?.all ?? []}
          isLoading={isFileListLoading}
          selectedFiles={selectedFiles}
          onSelectedFilesChange={setSelectedFiles}
        />,
      ],
    },
    {
      title: t("ExportModpackModal.label.exportOptions"),
      items: [
        // {
        //   title: t("ExportModpackModal.label.packWithLauncher"),
        //   children: (
        //     <Switch
        //       colorScheme={primaryColor}
        //       isChecked={packWithLauncher}
        //       onChange={(e) => setPackWithLauncher(e.target.checked)}
        //     />
        //   ),
        // },
        ...(isMultiMC
          ? [
              {
                title: t("ExportModpackModal.label.minMemory"),
                children: (
                  <HStack spacing={2}>
                    <NumberInput
                      min={0}
                      size="xs"
                      maxW={20}
                      clampValueOnBlur={false}
                      focusBorderColor={`${primaryColor}.500`}
                      value={minMemoryInput}
                      onChange={(value) => {
                        if (!/^\d*$/.test(value)) return;
                        setMinMemoryInput(value);
                      }}
                    >
                      <NumberInputField
                        pr={0}
                        fontSize="xs-sm"
                        placeholder={t("General.optional")}
                      />
                    </NumberInput>
                    <Text fontSize="xs" className="secondary-text">
                      MB
                    </Text>
                  </HStack>
                ),
              },
            ]
          : []),
        ...(isModrinth
          ? [
              {
                title: t("ExportModpackModal.label.noCreateRemoteFiles"),
                children: (
                  <Switch
                    colorScheme={primaryColor}
                    isChecked={noCreateRemoteFiles}
                    onChange={(e) => setNoCreateRemoteFiles(e.target.checked)}
                  />
                ),
              },
            ]
          : []),
      ].filter(Boolean) as OptionItemGroupProps["items"],
    },
  ];

  return (
    <Modal
      scrollBehavior="inside"
      size={{ base: "2xl", lg: "3xl", xl: "4xl" }}
      {...modalProps}
    >
      <ModalOverlay />
      <ModalContent maxH="calc(100vh - 7.5rem)">
        <ModalHeader>
          {t("ExportModpackModal.header.title", { param: instanceName })}
        </ModalHeader>
        <ModalCloseButton />
        <ModalBody>
          <VStack spacing={4} align="stretch">
            {optionGroups.map((group, index) => (
              <OptionItemGroup {...group} key={index} />
            ))}
          </VStack>
        </ModalBody>
        <ModalFooter>
          <HStack spacing={2}>
            <Button variant="ghost" onClick={modalProps.onClose}>
              {t("General.cancel")}
            </Button>
            <Button
              colorScheme={primaryColor}
              onClick={handleExport}
              isLoading={isLoading}
            >
              {t("ExportModpackModal.button.export")}
            </Button>
          </HStack>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default ExportModpackModal;
