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
import { openUrl } from "@tauri-apps/plugin-opener";
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
import { useGlobalData } from "@/contexts/global-data";
import { useSharedModals } from "@/contexts/shared-modal";
import { useTaskContext } from "@/contexts/task";
import { useToast } from "@/contexts/toast";
import {
  CampusModpackLine,
  CampusModpackProject,
  CampusModpackRecommendation,
} from "@/models/campus-modpack";
import { TaskTypeEnums } from "@/models/task";
import { CampusModpackService } from "@/services/campus-modpack";
import { registerPendingModpackOrigin } from "@/utils/modpack-origin";
import { sanitizeFileName } from "@/utils/string";

// ───── Campus modpack platform ─────

type CampusSelection = {
  project: CampusModpackProject;
  line: CampusModpackLine;
};

// ─── Followed instances ────────────────────────────────────────────────────

/** The instances that follow a modpack, with where each one stands. */
const FollowedInstancesSection = () => {
  const { t } = useTranslation();
  const { openSharedModal } = useSharedModals();
  const { getInstanceList, modpackChecks, checkModpackUpdate } =
    useGlobalData();
  const [checking, setChecking] = useState(false);

  const followed = (getInstanceList() ?? []).filter(
    (instance) => instance.origin
  );

  const handleCheckAll = useCallback(async () => {
    setChecking(true);
    for (const instance of followed) {
      await checkModpackUpdate(instance.id, true);
    }
    setChecking(false);
  }, [followed, checkModpackUpdate]);

  if (followed.length === 0) return null;

  return (
    <VStack align="stretch" spacing={3}>
      <HStack spacing={2} px={1} justify="space-between" w="100%">
        <Text fontSize="sm" fontWeight="bold">
          {t("InstallModpackPage.followed.title")}
        </Text>
        <CommonIconButton
          icon="refresh"
          label={t("InstallModpackPage.followed.checkAll")}
          withTooltip
          onClick={handleCheckAll}
          isDisabled={checking}
          size="xs"
          h={21}
        />
      </HStack>
      <OptionItemGroup
        items={followed.map((instance) => {
          const check = modpackChecks[instance.id];
          return {
            title: (
              <HStack spacing={2}>
                <Text>{instance.name}</Text>
                {check?.status === "available" && (
                  <Badge colorScheme="green">
                    {t("InstallModpackPage.followed.available")}
                  </Badge>
                )}
              </HStack>
            ),
            description: (
              <Text fontSize="xs" className="secondary-text">
                {[
                  instance.modpackVersion,
                  check
                    ? t(`UpdateModpackModal.status.${check.status}`, {
                        latest: check.latestLabel,
                        count: check.localChanges ?? 0,
                      })
                    : t("InstallModpackPage.followed.notChecked"),
                ]
                  .filter(Boolean)
                  .join(" · ")}
              </Text>
            ),
            children: (
              <CommonIconButton
                icon="update"
                label={t("InstallModpackPage.followed.open")}
                withTooltip
                size="xs"
                h={18}
                onClick={() =>
                  openSharedModal("update-modpack", {
                    instanceId: instance.id,
                    instanceName: instance.name,
                  })
                }
              />
            ),
          };
        })}
      />
    </VStack>
  );
};

/** The Modrinth project behind a recommend slot that lives off-campus. */
type ExternalProject = { title: string };

const CampusPlatformSection = () => {
  const { t } = useTranslation();
  const toast = useToast();
  const { handleScheduleProgressiveTaskGroup } = useTaskContext();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const installModal = useDisclosure();

  const [projects, setProjects] = useState<CampusModpackProject[]>([]);
  const [recommendation, setRecommendation] =
    useState<CampusModpackRecommendation | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [selected, setSelected] = useState<CampusSelection | null>(null);
  const [externalProject, setExternalProject] =
    useState<ExternalProject | null>(null);
  const [downloadingExternal, setDownloadingExternal] = useState(false);

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

  // The slot only carries the project id; ask Modrinth what it is called.
  const externalRef =
    recommendation?.platform === "modrinth" ? recommendation.projectRef : null;
  useEffect(() => {
    setExternalProject(null);
    if (!externalRef) return;
    let cancelled = false;
    fetch(
      `https://api.modrinth.com/v2/project/${encodeURIComponent(externalRef)}`
    )
      .then((response) => (response.ok ? response.json() : null))
      .then((data) => {
        if (!cancelled && data?.title)
          setExternalProject({ title: data.title });
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [externalRef]);

  /**
   * Installs the newest Modrinth version of the slot's project for the slot's
   * game version and loader, through the same download-then-import flow as any
   * other modpack download.
   */
  const handleInstallExternal = useCallback(
    async (rec: CampusModpackRecommendation) => {
      if (!rec.projectRef || !rec.gameVersion) return;
      setDownloadingExternal(true);
      try {
        const query = new URLSearchParams({
          game_versions: JSON.stringify([rec.gameVersion]),
        });
        if (rec.loaderType && rec.loaderType !== "vanilla")
          query.set("loaders", JSON.stringify([rec.loaderType]));
        let versions: any[] = [];
        try {
          const response = await fetch(
            `https://api.modrinth.com/v2/project/${encodeURIComponent(rec.projectRef)}/version?${query}`
          );
          if (!response.ok) throw new Error(String(response.status));
          const data = await response.json();
          versions = Array.isArray(data) ? data : [];
        } catch (error) {
          toast({
            title: t("General.networkError"),
            description: String(error),
            status: "warning",
          });
          return;
        }

        // Modrinth lists the newest first; prefer a release over a beta.
        const withPack = versions
          .map((version) => ({
            version,
            file:
              (version.files || []).find((f: any) => f.primary) ||
              (version.files || []).find((f: any) =>
                String(f.filename || "").endsWith(".mrpack")
              ),
          }))
          .filter((item) => item.file?.url);
        const picked =
          withPack.find((item) => item.version.version_type === "release") ||
          withPack[0];
        if (!picked) {
          toast({
            title: t("InstallModpackPage.platform.noVersion", {
              gameVersion: rec.gameVersion,
              loader: rec.loaderType || "-",
            }),
            status: "warning",
          });
          return;
        }

        const { version, file } = picked;
        const baseDir = await downloadDir();
        const fileName = sanitizeFileName(
          file.filename || `${version.id}.mrpack`
        );
        const savePath = `${baseDir}/${fileName}`;
        const sha1: string = file.hashes?.sha1 || "";
        // Only a published SHA-1 lets the backend prove the archive is this version.
        if (sha1) {
          registerPendingModpackOrigin(savePath, {
            platform: "modrinth",
            projectId: rec.projectRef,
            versionId: version.id,
            expectedSha1: sha1,
          });
        }
        handleScheduleProgressiveTaskGroup("modpack", [
          {
            src: file.url,
            dest: savePath,
            sha1,
            taskType: TaskTypeEnums.Download,
          },
        ]);
        toast({
          title: t("InstallModpackPage.platform.downloadStarted", {
            version: version.version_number || version.name,
          }),
          description: t("InstallModpackPage.platform.downloadDescription"),
          status: "success",
        });
      } finally {
        setDownloadingExternal(false);
      }
    },
    [handleScheduleProgressiveTaskGroup, t, toast]
  );

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
      <FollowedInstancesSection />
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
          {recommendation?.project && recommendation.line && (
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
          {recommendation?.projectRef && (
            <OptionItemGroup
              items={[
                {
                  title: (
                    <HStack spacing={2}>
                      <Text>
                        {externalProject?.title || recommendation.projectRef}
                      </Text>
                      <Badge colorScheme={primaryColor}>
                        {t("InstallModpackPage.platform.recommended")}
                      </Badge>
                    </HStack>
                  ),
                  description: (
                    <Text fontSize="xs" className="secondary-text">
                      {t("InstallModpackPage.platform.onOtherPlatform", {
                        platform: recommendation.platform,
                      })}
                    </Text>
                  ),
                  children:
                    recommendation.platform === "modrinth" ? (
                      <HStack spacing={1}>
                        <CommonIconButton
                          icon="external"
                          label={t("InstallModpackPage.platform.openOriginal")}
                          withTooltip
                          size="xs"
                          h={18}
                          onClick={() =>
                            openUrl(
                              `https://modrinth.com/project/${recommendation.projectRef}`
                            )
                          }
                        />
                        <CommonIconButton
                          icon="download"
                          label={t("General.download")}
                          withTooltip
                          size="xs"
                          h={18}
                          isDisabled={downloadingExternal}
                          onClick={() => handleInstallExternal(recommendation)}
                        />
                      </HStack>
                    ) : (
                      <CommonIconButton
                        icon="external"
                        label={t("InstallModpackPage.platform.openOriginal")}
                        withTooltip
                        size="xs"
                        h={18}
                        onClick={() =>
                          openUrl(
                            `https://www.curseforge.com/projects/${recommendation.projectRef}`
                          )
                        }
                      />
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
    router.query.tab === "cloud" || router.query.tab === "campus"
      ? router.query.tab
      : "campus"
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

      // Step 2: Schedule a progressive download task (same system as other modpack downloads)
      // The task will appear in the download arrow UI and trigger auto-install
      // when completed (since the group name is "modpack").
      // Use an absolute dest path so the file is saved to the Downloads folder
      // (same as any other modpack download) and the auto-install modal can find it.
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
