import {
  Avatar,
  Badge,
  Box,
  HStack,
  Link,
  Text,
  VStack,
} from "@chakra-ui/react";
import { invoke } from "@tauri-apps/api/core";
import { downloadDir } from "@tauri-apps/api/path";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { LuExternalLink } from "react-icons/lu";
import { BeatLoader } from "react-spinners";
import { CommonIconButton } from "@/components/common/common-icon-button";
import Empty from "@/components/common/empty";
import { OptionItemGroup } from "@/components/common/option-item";
import { Section } from "@/components/common/section";
import SegmentedControl from "@/components/common/segmented";
import { useLauncherConfig } from "@/contexts/config";
import { useTaskContext } from "@/contexts/task";
import { useToast } from "@/contexts/toast";
import { TaskTypeEnums } from "@/models/task";
import { sanitizeFileName } from "@/utils/string";

// ─── XPlus (Original Modpack) ──────────────────────────────────────────────

type XPlusVersion = {
  id: string;
  name: string;
  versionNumber: string;
  published: string;
  gameVersions: string[];
  downloadUrl: string;
  sha1: string;
  fileName: string;
};

const XPLUS_PROJECT_URL =
  "https://modrinth.com/modpack/xplus-2.0-modpack-global/versions";
const XPLUS_VERSION_API =
  "https://api.modrinth.com/v2/project/UCpApD3P/version";
const RECOMMENDED_VERSION = "1.21.11";
const MODPACK_ICON_URL =
  "https://cdn.modrinth.com/data/UCpApD3P/eb989b0763ca1ad11ee37e879ff2024294db410f_96.webp";
const RECOMMENDED_DOWNLOAD_URL =
  "https://cdn.modrinth.com/data/UCpApD3P/versions/hudo6QuU/XPlus%20PerioTable%20based%20on%20Minecraft%201.21.11%20%28Fabric%29.mrpack";

const fallbackVersions: XPlusVersion[] = [
  {
    id: "hudo6QuU",
    name: "XPlus PerioTable based on Minecraft 1.21.11 (Fabric)",
    versionNumber: "1.21.11",
    published: "",
    gameVersions: ["1.21.11"],
    downloadUrl: RECOMMENDED_DOWNLOAD_URL,
    sha1: "",
    fileName: "XPlus PerioTable based on Minecraft 1.21.11 (Fabric).mrpack",
  },
];

const toXPlusVersionList = (rawList: any[]): XPlusVersion[] => {
  const all = rawList
    .map((item) => {
      const primaryFile =
        (item.files || []).find((f: any) => f.primary) ||
        (item.files || []).find((f: any) => f.url?.endsWith(".mrpack"));
      if (!primaryFile?.url) return null;

      const encodedFileName =
        primaryFile.filename || item.name || `${item.id}.mrpack`;
      const decodedFileName = decodeURIComponent(encodedFileName);

      return {
        id: item.id,
        name: item.name || item.version_number || item.id,
        versionNumber: item.version_number || "",
        published: item.date_published || "",
        gameVersions: item.game_versions || [],
        downloadUrl: primaryFile.url,
        sha1: primaryFile.hashes?.sha1 || "",
        fileName: decodedFileName,
      } as XPlusVersion;
    })
    .filter((item): item is XPlusVersion => Boolean(item));

  // Deduplicate: keep only the latest published version per game version
  const latestByGameVersion = new Map<string, XPlusVersion>();
  for (const ver of all) {
    const primaryGameVer = ver.gameVersions[0] || ver.versionNumber;
    const existing = latestByGameVersion.get(primaryGameVer);
    if (
      !existing ||
      (ver.published &&
        existing.published &&
        ver.published > existing.published)
    ) {
      latestByGameVersion.set(primaryGameVer, ver);
    }
  }

  const deduped = Array.from(latestByGameVersion.values());

  // Sort: recommended version first, then by published date descending
  deduped.sort((a, b) => {
    const aRecommended = a.gameVersions.includes(RECOMMENDED_VERSION);
    const bRecommended = b.gameVersions.includes(RECOMMENDED_VERSION);
    if (aRecommended !== bRecommended) {
      return aRecommended ? -1 : 1;
    }
    if (!a.published || !b.published) return 0;
    return new Date(b.published).getTime() - new Date(a.published).getTime();
  });

  return deduped;
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
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;

  const [activeTab, setActiveTab] = useState<string>("xplus");

  const tabItems = useMemo(
    () => [
      {
        label: t("DiscoverLayout.discoverDomainList.original-modpack"),
        value: "xplus",
      },
      {
        label: t("DiscoverLayout.discoverDomainList.campus-modpack"),
        value: "campus",
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

        {activeTab === "xplus" ? (
          <XPlusModpackSection />
        ) : (
          <CampusModpackSection />
        )}
      </VStack>
    </Section>
  );
};

// ─── XPlus Section ──────────────────────────────────────────────────────────

const XPlusModpackSection = () => {
  const { t } = useTranslation();
  const toast = useToast();
  const { config } = useLauncherConfig();
  const { handleScheduleProgressiveTaskGroup } = useTaskContext();
  const primaryColor = config.appearance.theme.primaryColor;

  const [versions, setVersions] = useState<XPlusVersion[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [downloadingId, setDownloadingId] = useState<string>("");

  const fetchVersions = useCallback(async () => {
    setLoading(true);
    try {
      const response = await fetch(XPLUS_VERSION_API);
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const data = await response.json();
      const versionList = toXPlusVersionList(Array.isArray(data) ? data : []);
      setVersions(versionList.length > 0 ? versionList : fallbackVersions);
    } catch (_error) {
      setVersions(fallbackVersions);
      toast({
        title: t("General.networkError"),
        description: t("InstallModpackPage.xplus.loadFailed"),
        status: "warning",
      });
    } finally {
      setLoading(false);
    }
  }, [t, toast]);

  useEffect(() => {
    fetchVersions();
  }, [fetchVersions]);

  const recommendedVersionId = useMemo(() => {
    return versions.find((item) =>
      item.gameVersions.includes(RECOMMENDED_VERSION)
    )?.id;
  }, [versions]);

  const handleInstall = async (version: XPlusVersion) => {
    setDownloadingId(version.id);
    try {
      const baseDir = await downloadDir();
      const fileName = sanitizeFileName(
        version.fileName || `${version.name}.mrpack`
      );
      const savePath = `${baseDir}/${fileName}`;

      handleScheduleProgressiveTaskGroup("modpack", [
        {
          src: version.downloadUrl,
          dest: savePath,
          sha1: version.sha1,
          taskType: TaskTypeEnums.Download,
        },
      ]);

      toast({
        title: t("InstallModpackPage.xplus.downloadStarted", {
          version: version.versionNumber || version.name,
        }),
        description: t("InstallModpackPage.xplus.downloadDescription"),
        status: "success",
      });
    } finally {
      setDownloadingId("");
    }
  };

  return (
    <VStack align="stretch" spacing={3}>
      <Box px={1}>
        <HStack spacing={2} mb={1} justify="space-between" w="100%">
          <HStack spacing={2}>
            <Text fontSize="sm" fontWeight="bold">
              XPlus 2.0 Modpack (Global)
            </Text>
            <Link
              fontSize="xs"
              color={`${primaryColor}.500`}
              onClick={() => openUrl(XPLUS_PROJECT_URL)}
            >
              <HStack spacing={1}>
                <LuExternalLink />
                <Text>Modrinth</Text>
              </HStack>
            </Link>
          </HStack>
          <CommonIconButton
            icon="refresh"
            onClick={fetchVersions}
            isDisabled={loading}
            size="xs"
            h={21}
          />
        </HStack>
        <Text fontSize="xs" className="secondary-text">
          {t("InstallModpackPage.xplus.description", {
            version: RECOMMENDED_VERSION,
          })}
        </Text>
      </Box>

      {loading ? (
        <VStack my={8}>
          <BeatLoader size={14} color="gray" />
        </VStack>
      ) : versions.length === 0 ? (
        <Empty withIcon={false} size="sm" />
      ) : (
        <OptionItemGroup
          items={versions.map((version) => {
            const isRecommended = version.id === recommendedVersionId;
            return {
              title: (
                <HStack spacing={2}>
                  <Text>{version.versionNumber || version.name}</Text>
                  {isRecommended && (
                    <Badge colorScheme={primaryColor}>推荐</Badge>
                  )}
                </HStack>
              ),
              description: (
                <Text fontSize="xs" className="secondary-text">
                  {version.name}
                </Text>
              ),
              prefixElement: (
                <Avatar
                  src={MODPACK_ICON_URL}
                  name="XPlus"
                  boxSize={8}
                  borderRadius="md"
                />
              ),
              children: (
                <CommonIconButton
                  icon="download"
                  label={t("General.download")}
                  withTooltip
                  size="xs"
                  h={18}
                  isLoading={downloadingId === version.id}
                  onClick={() => handleInstall(version)}
                />
              ),
            };
          })}
        />
      )}
    </VStack>
  );
};

// ─── Campus Modpack Section ─────────────────────────────────────────────────

type AnyshareDownloadInfo = {
  method: string;
  url: string;
  headers: Record<string, string>;
  fileName: string;
};

const CampusModpackSection = () => {
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
