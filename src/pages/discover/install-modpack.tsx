import {
  Badge,
  Box,
  HStack,
  Text,
  VStack,
  useDisclosure,
} from "@chakra-ui/react";
import { invoke } from "@tauri-apps/api/core";
import { downloadDir } from "@tauri-apps/api/path";
import { useRouter } from "next/router";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { BeatLoader } from "react-spinners";
import { CommonIconButton } from "@/components/common/common-icon-button";
import Empty from "@/components/common/empty";
import { OptionItemGroup } from "@/components/common/option-item";
import { Section } from "@/components/common/section";
import SegmentedControl from "@/components/common/segmented";
import InstallCampusModpackModal from "@/components/modals/install-campus-modpack-modal";
import { useLauncherConfig } from "@/contexts/config";
import { useTaskContext } from "@/contexts/task";
import { useToast } from "@/contexts/toast";
import {
  CampusModpackLine,
  CampusModpackProject,
  CampusModpackRecommendation,
} from "@/models/campus-modpack";
import { TaskTypeEnums } from "@/models/task";
import { CampusModpackService } from "@/services/campus-modpack";
import { sanitizeFileName } from "@/utils/string";

// ───── Campus modpack platform ─────

type CampusSelection = {
  project: CampusModpackProject;
  line: CampusModpackLine;
};

const CampusPlatformSection = () => {
  const { t } = useTranslation();
  const toast = useToast();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const installModal = useDisclosure();

  const [projects, setProjects] = useState<CampusModpackProject[]>([]);
  const [recommendation, setRecommendation] =
    useState<CampusModpackRecommendation | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [selected, setSelected] = useState<CampusSelection | null>(null);

  const fetchAll = useCallback(async () => {
    setLoading(true);
    const [listResp, recommendResp] = await Promise.all([
      CampusModpackService.listCampusModpackProjects(),
      CampusModpackService.fetchCampusModpackRecommendation("recommended"),
    ]);
    if (listResp.status === "success") {
      setProjects(listResp.data.filter((project) => project.lines.length > 0));
    } else {
      setProjects([]);
      toast({
        title: listResp.message,
        description: listResp.details,
        status: "warning",
      });
    }
    setRecommendation(
      recommendResp.status === "success" ? recommendResp.data : null
    );
    setLoading(false);
  }, [toast]);

  useEffect(() => {
    fetchAll();
  }, [fetchAll]);

  const describeLine = useCallback(
    (line: CampusModpackLine) =>
      [
        line.gameVersion,
        line.loaderType === "vanilla"
          ? t("InstallModpackPage.platform.vanilla")
          : `${line.loaderType} ${line.loaderVersion}`,
        line.versionNumber,
      ].join(" · "),
    [t]
  );

  const handleSelect = (
    project: CampusModpackProject,
    line: CampusModpackLine
  ) => {
    setSelected({ project, line });
    installModal.onOpen();
  };

  const installButton = (
    project: CampusModpackProject,
    line: CampusModpackLine
  ) => (
    <CommonIconButton
      icon="download"
      label={t("General.download")}
      withTooltip
      size="xs"
      h={18}
      onClick={() => handleSelect(project, line)}
    />
  );

  return (
    <VStack align="stretch" spacing={3}>
      <Box px={1}>
        <HStack spacing={2} mb={1} justify="space-between" w="100%">
          <Text fontSize="sm" fontWeight="bold">
            {t("InstallModpackPage.platform.title")}
          </Text>
          <CommonIconButton
            icon="refresh"
            onClick={fetchAll}
            isDisabled={loading}
            size="xs"
            h={21}
          />
        </HStack>
        <Text fontSize="xs" className="secondary-text">
          {t("InstallModpackPage.platform.description")}
        </Text>
      </Box>

      {loading ? (
        <VStack my={8}>
          <BeatLoader size={14} color="gray" />
        </VStack>
      ) : (
        <>
          {recommendation && (
            <OptionItemGroup
              items={[
                {
                  title: (
                    <HStack spacing={2}>
                      <Text>{recommendation.project.name}</Text>
                      <Badge colorScheme={primaryColor}>
                        {t("InstallModpackPage.platform.recommended")}
                      </Badge>
                    </HStack>
                  ),
                  description: (
                    <Text fontSize="xs" className="secondary-text">
                      {describeLine(recommendation.line)}
                    </Text>
                  ),
                  children: installButton(
                    recommendation.project,
                    recommendation.line
                  ),
                },
              ]}
            />
          )}
          {projects.length === 0 ? (
            <Empty withIcon={false} size="sm" />
          ) : (
            projects.map((project) => (
              <OptionItemGroup
                key={project.id}
                title={project.name}
                description={project.summary || undefined}
                items={project.lines.map((line) => ({
                  title: describeLine(line),
                  children: installButton(project, line),
                }))}
              />
            ))
          )}
        </>
      )}

      {selected && (
        <InstallCampusModpackModal
          key={`${selected.project.id}-${selected.line.id}`}
          project={selected.project}
          line={selected.line}
          isOpen={installModal.isOpen}
          onClose={installModal.onClose}
        />
      )}
    </VStack>
  );
};

// ─── Campus Modpack (Anyshare) ──────────────────────────────────────────────

type AnyshareFileItem = {
  docid: string;
  name: string;
  size: number | null;
  isDir: boolean;
};

const ANYSHARE_URL =
  "https://yunpan.ustb.edu.cn/link/AA96B8A02265AB4439ACB0027CA5A19225";

const formatFileSize = (bytes: number | null): string => {
  if (bytes === null || bytes === undefined) return "-";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  for (const unit of units) {
    if (value < 1024 || unit === "TB") {
      return unit === "B"
        ? `${Math.round(value)} ${unit}`
        : `${value.toFixed(2)} ${unit}`;
    }
    value /= 1024;
  }
  return `${value.toFixed(2)} TB`;
};

// ─── Main Page ──────────────────────────────────────────────────────────────

const InstallModpackPage = () => {
  const { t } = useTranslation();
  const router = useRouter();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;

  const [activeTab, setActiveTab] = useState<string>(
    router.query.tab === "cloud" ? "cloud" : "campus"
  );

  useEffect(() => {
    if (router.query.tab === "campus" || router.query.tab === "cloud")
      setActiveTab(router.query.tab);
  }, [router.query.tab]);

  const tabItems = useMemo(
    () => [
      {
        label: t("DiscoverLayout.discoverDomainList.campus-modpack"),
        value: "campus",
      },
      {
        label: t("DiscoverLayout.discoverDomainList.cloud-modpack"),
        value: "cloud",
      },
    ],
    [t]
  );

  return (
    <Section
      title={t("DiscoverLayout.discoverDomainList.install-modpack")}
      w="100%"
      h="100%"
      display="flex"
      flexDir="column"
    >
      <VStack align="stretch" spacing={3}>
        <Box px={1}>
          <SegmentedControl
            size="sm"
            items={tabItems}
            selected={activeTab}
            onSelectItem={setActiveTab}
            colorScheme={primaryColor}
          />
        </Box>

        {activeTab === "campus" ? (
          <CampusPlatformSection />
        ) : (
          <CloudModpackSection />
        )}
      </VStack>
    </Section>
  );
};

// ─── Campus Modpack Section ─────────────────────────────────────────────────

type AnyshareDownloadInfo = {
  method: string;
  url: string;
  headers: Record<string, string>;
  fileName: string;
};

const CloudModpackSection = () => {
  const { t } = useTranslation();
  const toast = useToast();
  const { handleScheduleProgressiveTaskGroup } = useTaskContext();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;

  const [files, setFiles] = useState<AnyshareFileItem[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [downloadingId, setDownloadingId] = useState<string>("");

  const fetchFiles = useCallback(async () => {
    setLoading(true);
    try {
      const result = await invoke<AnyshareFileItem[]>(
        "fetch_anyshare_folder_list",
        { shareUrl: ANYSHARE_URL }
      );
      // Filter to show only files (not directories)
      setFiles(result.filter((f) => !f.isDir));
    } catch (error) {
      setFiles([]);
      toast({
        title: t("General.networkError"),
        description: t("InstallModpackPage.campus.loadFailed", {
          error: String(error),
        }),
        status: "warning",
      });
    } finally {
      setLoading(false);
    }
  }, [t, toast]);

  useEffect(() => {
    fetchFiles();
  }, [fetchFiles]);

  const handleInstall = async (file: AnyshareFileItem) => {
    setDownloadingId(file.docid);
    try {
      // Step 1: Get the download URL and required headers from Anyshare
      const downloadInfo = await invoke<AnyshareDownloadInfo>(
        "fetch_anyshare_download_url",
        {
          shareUrl: ANYSHARE_URL,
          docid: file.docid,
          fileName: file.name,
        }
      );

      // Step 2: Schedule a progressive download task (same system as XPlus)
      // The task will appear in the download arrow UI and trigger auto-install
      // when completed (since the group name is "modpack").
      // Use an absolute dest path so the file is saved to the Downloads folder
      // (same as XPlus) and the auto-install modal can find it.
      const baseDir = await downloadDir();
      const fileName = sanitizeFileName(file.name);
      const savePath = `${baseDir}/${fileName}`;

      handleScheduleProgressiveTaskGroup("modpack", [
        {
          src: downloadInfo.url,
          dest: savePath,
          customHeaders: downloadInfo.headers,
          taskType: TaskTypeEnums.Download,
        },
      ]);

      toast({
        title: t("InstallModpackPage.campus.downloadStarted", {
          name: file.name,
        }),
        description: t("InstallModpackPage.campus.downloadDescription"),
        status: "success",
      });
    } catch (error) {
      toast({
        title: t("InstallModpackPage.campus.downloadFailed"),
        description: String(error),
        status: "error",
      });
    } finally {
      setDownloadingId("");
    }
  };

  return (
    <VStack align="stretch" spacing={3}>
      <Box px={1}>
        <HStack spacing={2} mb={1} justify="space-between" w="100%">
          <Text fontSize="sm" fontWeight="bold">
            {t("InstallModpackPage.campus.title")}
          </Text>
          <CommonIconButton
            icon="refresh"
            onClick={fetchFiles}
            isDisabled={loading}
            size="xs"
            h={21}
          />
        </HStack>
        <Text fontSize="xs" className="secondary-text">
          {t("InstallModpackPage.campus.description")}
        </Text>
      </Box>

      {loading ? (
        <VStack my={8}>
          <BeatLoader size={14} color="gray" />
        </VStack>
      ) : files.length === 0 ? (
        <Empty withIcon={false} size="sm" />
      ) : (
        <OptionItemGroup
          items={files.map((file) => ({
            title: <Text fontSize="xs-sm">{file.name}</Text>,
            description: (
              <Text fontSize="xs" className="secondary-text">
                {formatFileSize(file.size)}
              </Text>
            ),
            children: (
              <CommonIconButton
                icon="download"
                label={t("General.download")}
                withTooltip
                size="xs"
                h={18}
                isLoading={downloadingId === file.docid}
                onClick={() => handleInstall(file)}
              />
            ),
          }))}
        />
      )}
    </VStack>
  );
};

export default InstallModpackPage;
