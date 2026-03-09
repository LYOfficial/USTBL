import {
  Avatar,
  Badge,
  Box,
  HStack,
  Link,
  Text,
  VStack,
} from "@chakra-ui/react";
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
import { useLauncherConfig } from "@/contexts/config";
import { useTaskContext } from "@/contexts/task";
import { useToast } from "@/contexts/toast";
import { TaskTypeEnums } from "@/models/task";
import { sanitizeFileName } from "@/utils/string";

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
    fileName:
      "XPlus PerioTable based on Minecraft 1.21.11 (Fabric).mrpack",
  },
];

const toXPlusVersionList = (rawList: any[]): XPlusVersion[] => {
  return rawList
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
    .filter((item): item is XPlusVersion => Boolean(item))
    .sort((a, b) => {
      if (!a.published || !b.published) return 0;
      return new Date(b.published).getTime() - new Date(a.published).getTime();
    });
};

const InstallModpackPage = () => {
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
        description: "Modrinth 版本列表加载失败，已使用推荐版本。",
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
        title: `已开始下载 ${version.versionNumber || version.name}`,
        description: "下载完成后会自动进入整合包安装流程。",
        status: "success",
      });
    } finally {
      setDownloadingId("");
    }
  };

  return (
    <Section
      title={t("DiscoverLayout.discoverDomainList.install-modpack")}
      w="100%"
      h="100%"
      display="flex"
      flexDir="column"
      headExtra={
        <CommonIconButton
          icon="refresh"
          onClick={fetchVersions}
          isDisabled={loading}
          size="xs"
          h={21}
        />
      }
    >
      <VStack align="stretch" spacing={3}>
        <Box px={1}>
          <HStack spacing={2} mb={1}>
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
          <Text fontSize="xs" className="secondary-text">
            仅保留 XPlus 各版本安装。默认推荐下载 Minecraft
            {" "}
            {RECOMMENDED_VERSION}
            版本。
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
                    src="/images/icons/Logo_128x128.png"
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
    </Section>
  );
};

export default InstallModpackPage;
