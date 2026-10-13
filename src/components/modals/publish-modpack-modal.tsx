import {
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
import { formatByteSize } from "@/utils/string";

interface PublishModpackModalProps extends Omit<ModalProps, "children"> {
  instanceId: string;
  instanceName: string;
}

const NEW_PROJECT = "__new__";
const SLUG_PATTERN = /^[a-z][a-z0-9-]{1,63}$/;

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
  const [slug, setSlug] = useState("");
  const [projectName, setProjectName] = useState(instanceName);
  const [summary, setSummary] = useState("");
  const [versionNumber, setVersionNumber] = useState("");
  const [changelog, setChangelog] = useState("");
  const [fileList, setFileList] = useState<ModpackFileList | null>(null);
  const [selectedFiles, setSelectedFiles] = useState<Set<string>>(new Set());
  const [isFileListLoading, setIsFileListLoading] = useState(false);
  const [isPublishing, setIsPublishing] = useState(false);
  const [progress, setProgress] = useState<CampusPublishProgress | null>(null);

  const isNewProject = projectChoice === NEW_PROJECT;

  const checkSlugError = useCallback((value: string): number => {
    if (!SLUG_PATTERN.test(value.trim().toLowerCase())) return 1;
    return 0;
  }, []);

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
      .finally(() => {
        if (isActive) setIsFileListLoading(false);
      });

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
      (checkSlugError(slug) !== 0 ||
        checkNameError(projectName) !== 0 ||
        checkSummaryError(summary) !== 0)
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
      let projectRef = projectChoice;
      if (isNewProject) {
        const created = await CampusModpackService.createCampusModpackProject(
          slug.trim().toLowerCase(),
          projectName.trim(),
          summary.trim()
        );
        if (created.status !== "success") {
          toast({
            title: created.message,
            description: created.details,
            status: "error",
          });
          return;
        }
        projectRef = created.data.slug;
        setProjects((prev) => [...prev, created.data]);
        setProjectChoice(created.data.slug);
      }

      const response = await CampusModpackService.publishCampusModpack(
        instanceId,
        projectRef,
        versionNumber.trim(),
        changelog,
        selectedFileList
      );
      if (response.status === "success") {
        if (response.data.adoption === "followed") getInstanceList(true);
        const notes = [
          response.data.skipped.length > 0
            ? t("PublishModpackModal.skipped", {
                count: response.data.skipped.length,
              })
            : undefined,
          response.data.adoption === "followed"
            ? undefined
            : t(`PublishModpackModal.adoption.${response.data.adoption}`),
        ].filter(Boolean);
        toast({
          title: response.message,
          description: notes.length > 0 ? notes.join(" ") : undefined,
          status: "success",
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
    checkSlugError,
    checkSummaryError,
    checkVersionError,
    getInstanceList,
    instanceId,
    isNewProject,
    modalProps,
    projectChoice,
    projectName,
    selectedFileList,
    slug,
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
                title: t("PublishModpackModal.label.slug"),
                children: (
                  <Editable
                    isTextArea={false}
                    value={slug}
                    onEditSubmit={setSlug}
                    checkError={checkSlugError}
                    localeKey="PublishModpackModal.errorMessage.slug"
                    {...editableProps}
                  />
                ),
              },
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
            {progress?.transfer && (
              <VStack spacing={1} align="stretch">
                <Text fontSize="xs" className="secondary-text">
                  {t("PublishModpackModal.transfer.summary", {
                    done: formatByteSize(progress.transfer.bytesDone),
                    total: formatByteSize(progress.transfer.bytesTotal),
                    speed: formatByteSize(progress.transfer.speed),
                    concurrency: progress.transfer.concurrency,
                  })}
                </Text>
                {progress.transfer.files.map((file, index) => (
                  <HStack key={`${file.name}-${index}`} spacing={2}>
                    <Text fontSize="xs" noOfLines={1} flex="1" minW={0}>
                      {file.name}
                    </Text>
                    <Progress
                      size="xs"
                      w="96px"
                      borderRadius="full"
                      value={file.total ? (file.done / file.total) * 100 : 100}
                    />
                    <Text
                      fontSize="xs"
                      className="secondary-text"
                      w="150px"
                      textAlign="right"
                      flexShrink={0}
                    >
                      {formatByteSize(file.done)} / {formatByteSize(file.total)}
                    </Text>
                  </HStack>
                ))}
              </VStack>
            )}
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
