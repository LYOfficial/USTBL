import {
  Alert,
  AlertIcon,
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
  Select,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import Editable from "@/components/common/editable";
import ModpackFileTree from "@/components/common/modpack-file-tree";
import {
  OptionItemGroup,
  OptionItemGroupProps,
} from "@/components/common/option-item";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import {
  CampusModpackProject,
  CampusPublishProgress,
} from "@/models/campus-modpack";
import { ModpackFileList } from "@/models/instance/misc";
import { CampusModpackService } from "@/services/campus-modpack";
import { InstanceService } from "@/services/instance";

interface PublishModpackModalProps extends Omit<ModalProps, "children"> {
  instanceId: string;
  instanceName: string;
}

const NEW_PROJECT = "__new__";
const DISABLED_SUFFIX = ".disabled";

const PublishModpackModal: React.FC<PublishModpackModalProps> = ({
  instanceId,
  instanceName,
  ...modalProps
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const toast = useToast();
  const { getInstanceList } = useGlobalData();
  const primaryColor = config.appearance.theme.primaryColor;

  const [projects, setProjects] = useState<CampusModpackProject[]>([]);
  const [projectChoice, setProjectChoice] = useState(NEW_PROJECT);
  const [projectName, setProjectName] = useState(instanceName);
  const [summary, setSummary] = useState("");
  const [versionNumber, setVersionNumber] = useState("");
  const [changelog, setChangelog] = useState("");
  const [fileList, setFileList] = useState<ModpackFileList | null>(null);
  const [selectedFiles, setSelectedFiles] = useState<Set<string>>(new Set());
  const [isFileListLoading, setIsFileListLoading] = useState(false);
  const [isPublishing, setIsPublishing] = useState(false);
  const [progress, setProgress] = useState<CampusPublishProgress | null>(null);
  // The publishability gate: "unknown" while the check is in flight, a reason
  // string when the instance cannot be published, "" when it passed.
  const [publishBlockReason, setPublishBlockReason] = useState<
    string | "unknown"
  >("unknown");

  const isNewProject = projectChoice === NEW_PROJECT;

  const checkNameError = useCallback((value: string): number => {
    const trimmed = value.trim();
    if (trimmed === "") return 1;
    if (trimmed.length > 128) return 2;
    return 0;
  }, []);

  const checkSummaryError = useCallback((value: string): number => {
    return value.length > 512 ? 1 : 0;
  }, []);

  const checkVersionError = useCallback((value: string): number => {
    const trimmed = value.trim();
    if (trimmed === "") return 1;
    if (trimmed.length > 64) return 2;
    return 0;
  }, []);

  const checkChangelogError = useCallback((value: string): number => {
    return value.length > 20000 ? 1 : 0;
  }, []);

  useEffect(() => {
    if (!modalProps.isOpen) return;
    let isActive = true;
    setPublishBlockReason("unknown");

    CampusModpackService.checkCampusModpackPublishable(instanceId).then(
      (response) => {
        if (!isActive) return;
        if (response.status !== "success") {
          setPublishBlockReason(response.details || response.message);
          return;
        }
        setPublishBlockReason("");

        CampusModpackService.listCampusModpackProjects().then((response) => {
          if (!isActive) return;
          if (response.status === "success") {
            setProjects(response.data);
          } else {
            toast({
              title: response.message,
              description: response.details,
              status: "error",
            });
          }
        });

        setIsFileListLoading(true);
        InstanceService.retrieveExportableFileList(instanceId)
          .then((response) => {
            if (!isActive) return;
            if (response.status === "success") {
              const all = response.data.all.filter(
                (path) => !path.toLowerCase().endsWith(DISABLED_SUFFIX)
              );
              setFileList({ ...response.data, all });
              const nextSelected = new Set(all);
              response.data.unchecked.forEach((path) =>
                nextSelected.delete(path)
              );
              setSelectedFiles(nextSelected);
            } else {
              toast({
                title: response.message,
                description: response.details,
                status: "error",
              });
            }
          })
          .finally(() => {
            if (isActive) setIsFileListLoading(false);
          });
      }
    );

    return () => {
      isActive = false;
    };
  }, [instanceId, modalProps.isOpen, toast]);

  // Keep what the line's current release lists selected: the dialog opens with
  // the exporter's defaults, which can leave such files out.
  useEffect(() => {
    if (!modalProps.isOpen || isNewProject || !fileList) return;
    let isActive = true;
    CampusModpackService.fetchCampusModpackPreviousPaths(
      instanceId,
      projectChoice
    ).then((response) => {
      if (!isActive || response.status !== "success") return;
      const known = new Set(fileList.all);
      setSelectedFiles((prev) => {
        const next = new Set(prev);
        response.data
          .filter((path) => known.has(path))
          .forEach((path) => next.add(path));
        return next;
      });
    });
    return () => {
      isActive = false;
    };
  }, [fileList, instanceId, isNewProject, modalProps.isOpen, projectChoice]);

  useEffect(() => {
    if (!modalProps.isOpen) return;
    return CampusModpackService.onPublishProgress((payload) => {
      if (payload.instanceId === instanceId) setProgress(payload);
    });
  }, [instanceId, modalProps.isOpen]);

  const selectedFileList = useMemo(() => {
    if (!fileList) return [];
    return fileList.all.filter((path) => selectedFiles.has(path));
  }, [fileList, selectedFiles]);

  const handlePublish = useCallback(async () => {
    if (checkVersionError(versionNumber) !== 0) {
      toast({
        title: t("PublishModpackModal.error.invalidVersion"),
        status: "warning",
      });
      return;
    }
    if (selectedFileList.length === 0) {
      toast({
        title: t("PublishModpackModal.error.noFileSelected"),
        status: "warning",
      });
      return;
    }
    if (
      isNewProject &&
      (checkNameError(projectName) !== 0 || checkSummaryError(summary) !== 0)
    ) {
      toast({
        title: t("PublishModpackModal.error.invalidProject"),
        status: "warning",
      });
      return;
    }

    setIsPublishing(true);
    setProgress(null);
    try {
      // The platform names a new project itself and returns its identifier.
      const response = await CampusModpackService.publishCampusModpack(
        instanceId,
        isNewProject ? null : projectChoice,
        isNewProject
          ? { name: projectName.trim(), summary: summary.trim() }
          : null,
        versionNumber.trim(),
        changelog,
        selectedFileList
      );
      if (response.status === "success") {
        // The upload goes on in the background; its result is told when done.
        toast({
          title: t("PublishModpackModal.started.title"),
          description: t("PublishModpackModal.started.description"),
          status: "info",
        });
        modalProps.onClose();
      } else {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
    } finally {
      setIsPublishing(false);
      setProgress(null);
    }
  }, [
    changelog,
    checkNameError,
    checkSummaryError,
    checkVersionError,
    instanceId,
    isNewProject,
    modalProps,
    projectChoice,
    projectName,
    selectedFileList,
    summary,
    t,
    toast,
    versionNumber,
  ]);

  const editableProps = {
    textProps: { className: "secondary-text", fontSize: "xs-sm" },
    inputProps: { fontSize: "xs-sm" },
    formErrMsgProps: { fontSize: "xs-sm" },
  };

  const optionGroups: OptionItemGroupProps[] = [
    {
      title: t("PublishModpackModal.label.project"),
      items: [
        ...(publishBlockReason !== "" && publishBlockReason !== "unknown"
          ? [
              {
                title: "",
                children: (
                  <Alert status="error" borderRadius="md">
                    <AlertIcon />
                    {publishBlockReason}
                  </Alert>
                ),
              },
            ]
          : []),
        {
          title: t("PublishModpackModal.label.target"),
          children: (
            <Select
              size="xs"
              w="auto"
              minW="48"
              value={projectChoice}
              aria-label={t("PublishModpackModal.label.target")}
              isDisabled={isPublishing}
              onChange={(event) => setProjectChoice(event.target.value)}
            >
              <option value={NEW_PROJECT}>
                {t("PublishModpackModal.label.newProject")}
              </option>
              {projects.map((project) => (
                <option key={project.id} value={project.slug}>
                  {project.name}
                </option>
              ))}
            </Select>
          ),
        },
        ...(isNewProject
          ? [
              {
                title: t("PublishModpackModal.label.projectName"),
                children: (
                  <Editable
                    isTextArea={false}
                    value={projectName}
                    onEditSubmit={setProjectName}
                    checkError={checkNameError}
                    localeKey="PublishModpackModal.errorMessage.name"
                    {...editableProps}
                  />
                ),
              },
              {
                title: t("PublishModpackModal.label.summary"),
                children: (
                  <Editable
                    isTextArea={true}
                    value={summary}
                    onEditSubmit={setSummary}
                    checkError={checkSummaryError}
                    localeKey="PublishModpackModal.errorMessage.summary"
                    {...editableProps}
                    inputProps={{
                      fontSize: "xs-sm",
                      placeholder: t("General.optional"),
                    }}
                  />
                ),
              },
            ]
          : []),
      ],
    },
    {
      title: t("PublishModpackModal.label.version"),
      items: [
        {
          title: t("PublishModpackModal.label.versionNumber"),
          children: (
            <Editable
              isTextArea={false}
              value={versionNumber}
              onEditSubmit={setVersionNumber}
              checkError={checkVersionError}
              localeKey="PublishModpackModal.errorMessage.version"
              {...editableProps}
            />
          ),
        },
        {
          title: t("PublishModpackModal.label.changelog"),
          children: (
            <Editable
              isTextArea={true}
              value={changelog}
              onEditSubmit={setChangelog}
              checkError={checkChangelogError}
              localeKey="PublishModpackModal.errorMessage.changelog"
              {...editableProps}
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
      title: t("PublishModpackModal.label.includedFiles"),
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
  ];

  return (
    <Modal
      scrollBehavior="inside"
      size={{ base: "2xl", lg: "3xl", xl: "4xl" }}
      closeOnOverlayClick={!isPublishing}
      {...modalProps}
    >
      <ModalOverlay />
      <ModalContent maxH="calc(100vh - 7.5rem)">
        <ModalHeader>
          {t("PublishModpackModal.header.title", { param: instanceName })}
        </ModalHeader>
        <ModalCloseButton isDisabled={isPublishing} />
        <ModalBody>
          <VStack spacing={4} align="stretch">
            {optionGroups.map((group, index) => (
              <OptionItemGroup {...group} key={index} />
            ))}
          </VStack>
        </ModalBody>
        <ModalFooter>
          <HStack spacing={2}>
            {progress && (
              <Text fontSize="xs" className="secondary-text">
                {t(`PublishModpackModal.stage.${progress.stage}`, {
                  done: progress.done,
                  total: progress.total,
                })}
              </Text>
            )}
            <Button
              variant="ghost"
              onClick={modalProps.onClose}
              isDisabled={isPublishing}
            >
              {t("General.cancel")}
            </Button>
            <Button
              colorScheme={primaryColor}
              onClick={handlePublish}
              isLoading={isPublishing}
              isDisabled={publishBlockReason !== ""}
            >
              {t("PublishModpackModal.button.publish")}
            </Button>
          </HStack>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default PublishModpackModal;
