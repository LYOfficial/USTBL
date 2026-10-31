import {
  Badge,
  Box,
  HStack,
  Text,
  Tooltip,
  VStack,
  useDisclosure,
} from "@chakra-ui/react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { BeatLoader } from "react-spinners";
import { CommonIconButton } from "@/components/common/common-icon-button";
import Empty from "@/components/common/empty";
import { OptionItemGroup } from "@/components/common/option-item";
import InstallCampusModpackModal from "@/components/modals/install-campus-modpack-modal";
import { useLauncherConfig } from "@/contexts/config";
import { useSharedModals } from "@/contexts/shared-modal";
import { useToast } from "@/contexts/toast";
import { OtherResourceSource } from "@/enums/resource";
import {
  CampusModpackLine,
  CampusModpackProject,
  CampusModpackRecommendation,
} from "@/models/campus-modpack";
import { InstanceSummary } from "@/models/instance/misc";
import { VustbAccount, isVustbAdmin } from "@/models/vustb";
import { AccountService } from "@/services/account";
import { CampusModpackService } from "@/services/campus-modpack";
import { campusProjectRefs, findInstancesOfLine } from "@/utils/modpack-ledger";

type CampusSelection = {
  project: CampusModpackProject;
  line: CampusModpackLine;
};

/** The Modrinth project behind a recommend slot that lives off-campus. */
type ExternalProject = { title: string; id?: string; slug?: string };

export interface CampusPlatformSectionProps {
  projects: CampusModpackProject[];
  recommendation: CampusModpackRecommendation | null;
  loading: boolean;
  onRefresh: () => void;
  /** Local instances, to mark what is already installed. */
  instances: InstanceSummary[];
  /** Jumps to the ledger row of an instance. */
  onFocusInstance: (instanceId: string) => void;
}

export const CampusPlatformSection: React.FC<CampusPlatformSectionProps> = ({
  projects,
  recommendation,
  loading,
  onRefresh,
  instances,
  onFocusInstance,
}) => {
  const { t } = useTranslation();
  const toast = useToast();
  const { requireVustbAccount, openGenericConfirmDialog, openSharedModal } =
    useSharedModals();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const installModal = useDisclosure();

  const [vustbAccount, setVustbAccount] = useState<VustbAccount | null>(null);
  const [deletingId, setDeletingId] = useState<number | null>(null);
  useEffect(() => {
    AccountService.retrieveVustbAccount().then((response) => {
      if (response.status === "success") setVustbAccount(response.data);
    });
  }, []);
  const isAdmin = isVustbAdmin(vustbAccount);

  const [selected, setSelected] = useState<CampusSelection | null>(null);
  const [externalProject, setExternalProject] =
    useState<ExternalProject | null>(null);
  const [downloadingExternal, setDownloadingExternal] = useState(false);

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
          setExternalProject({
            title: data.title,
            id: data.id,
            slug: data.slug,
          });
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [externalRef]);

  /**
   * Installs the newest Modrinth version of the slot's project for the slot's
   * game version and loader, through the same download-then-import flow as any
   * platform installation flow.
   */
  const handleInstallExternalAllowed = useCallback(
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
            title: t("InstanceSyncPage.platform.noVersion", {
              gameVersion: rec.gameVersion,
              loader: rec.loaderType || "-",
            }),
            status: "warning",
          });
          return;
        }

        const { version, file } = picked;
        const sha1: string = file.hashes?.sha1 || "";
        if (!sha1) {
          toast({
            title: t("DownloadSpecificResourceModal.error.missingSha1"),
            status: "error",
          });
          return;
        }
        openSharedModal("install-platform-modpack", {
          projectName: externalProject?.title || rec.projectRef,
          projectDescription: "",
          projectIconSrc: "",
          projectId: rec.projectRef,
          source: OtherResourceSource.Modrinth,
          file: {
            resourceId: rec.projectRef,
            fileId: version.id,
            name: version.name || version.id,
            releaseType: version.version_type || "release",
            downloads: 0,
            fileDate: version.date_published || "",
            downloadUrl: file.url,
            sha1,
            fileName: file.filename || `${version.id}.mrpack`,
            dependencies: [],
            loader: rec.loaderType || undefined,
          },
          gameVersion: rec.gameVersion,
        });
      } finally {
        setDownloadingExternal(false);
      }
    },
    [externalProject, openSharedModal, t, toast]
  );

  const handleInstallExternal = useCallback(
    async (rec: CampusModpackRecommendation) => {
      if (!rec.projectRef || !rec.gameVersion) return;
      requireVustbAccount(() => {
        void handleInstallExternalAllowed(rec);
      });
    },
    [handleInstallExternalAllowed, requireVustbAccount]
  );

  const describeLine = useCallback(
    (line: CampusModpackLine) =>
      [
        line.gameVersion,
        line.loaderType === "vanilla"
          ? t("InstanceSyncPage.platform.vanilla")
          : `${line.loaderType} ${line.loaderVersion}`,
        line.versionNumber,
      ].join(" · "),
    [t]
  );

  /** The newest version's changelog; older platforms send none. */
  const changelogOf = (line: CampusModpackLine) => line.changelog?.trim() ?? "";

  const handleSelect = (
    project: CampusModpackProject,
    line: CampusModpackLine
  ) => {
    setSelected({ project, line });
    requireVustbAccount(installModal.onOpen);
  };

  /** Marks a line that already has a local instance; click jumps to its row. */
  const installedBadge = (matches: InstanceSummary[]) =>
    matches.length > 0 && (
      <Badge
        as="button"
        type="button"
        colorScheme="green"
        cursor="pointer"
        title={t("InstanceSyncPage.ledger.installedHint", {
          count: matches.length,
        })}
        onClick={() => onFocusInstance(matches[0].id)}
      >
        {t("InstanceSyncPage.ledger.installed")}
      </Badge>
    );

  const installedOfCampusLine = (
    project: CampusModpackProject,
    line: CampusModpackLine
  ) =>
    findInstancesOfLine(instances, {
      platform: "campus",
      projectRefs: campusProjectRefs(project),
      gameVersion: line.gameVersion,
      loaderType: line.loaderType,
    });

  const installButton = (
    project: CampusModpackProject,
    line: CampusModpackLine
  ) => (
    <HStack spacing={1.5}>
      {installedBadge(installedOfCampusLine(project, line))}
      <CommonIconButton
        icon="download"
        label={t("General.download")}
        withTooltip
        size="xs"
        h={18}
        onClick={() => handleSelect(project, line)}
      />
    </HStack>
  );

  const handleDeleteProject = useCallback(
    async (project: CampusModpackProject) => {
      setDeletingId(project.id);
      try {
        const response = await CampusModpackService.deleteCampusModpackProject(
          project.slug
        );
        toast({
          title: response.message,
          description: response.status === "error" ? response.details : "",
          status: response.status,
        });
        if (response.status === "success") onRefresh();
      } finally {
        setDeletingId(null);
      }
    },
    [onRefresh, toast]
  );

  const deleteButton = (project: CampusModpackProject) =>
    isAdmin && (
      <CommonIconButton
        icon="delete"
        label={t("InstanceSyncPage.platform.delete")}
        withTooltip
        colorScheme="red"
        size="xs"
        h={21}
        isLoading={deletingId === project.id}
        isDisabled={deletingId !== null}
        onClick={() =>
          openGenericConfirmDialog({
            title: t("InstanceSyncPage.platform.deleteDialog.title"),
            body: t("InstanceSyncPage.platform.deleteDialog.content", {
              name: project.name,
            }),
            btnOK: t("General.delete"),
            isAlert: true,
            onOKCallback: () => {
              void handleDeleteProject(project);
            },
          })
        }
      />
    );

  const installedOfExternal = (rec: CampusModpackRecommendation) =>
    rec.platform === "modrinth" || rec.platform === "curseforge"
      ? findInstancesOfLine(instances, {
          platform: rec.platform,
          projectRefs: [
            rec.projectRef,
            externalProject?.id,
            externalProject?.slug,
          ].filter((ref): ref is string => !!ref),
          gameVersion: rec.gameVersion,
          loaderType: rec.loaderType,
        })
      : [];

  return (
    <VStack align="stretch" spacing={3}>
      <Box px={1}>
        <HStack spacing={2} mb={1} justify="space-between" w="100%">
          <Text fontSize="sm" fontWeight="bold">
            {t("InstanceSyncPage.platform.title")}
          </Text>
          <CommonIconButton
            icon="refresh"
            onClick={onRefresh}
            isDisabled={loading}
            size="xs"
            h={21}
          />
        </HStack>
        <Text fontSize="xs" className="secondary-text">
          {t("InstanceSyncPage.platform.description")}
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
                        {t("InstanceSyncPage.platform.recommended")}
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
                        {t("InstanceSyncPage.platform.recommended")}
                      </Badge>
                    </HStack>
                  ),
                  description: (
                    <Text fontSize="xs" className="secondary-text">
                      {t("InstanceSyncPage.platform.onOtherPlatform", {
                        platform: recommendation.platform,
                      })}
                    </Text>
                  ),
                  children:
                    recommendation.platform === "modrinth" ? (
                      <HStack spacing={1.5}>
                        {installedBadge(installedOfExternal(recommendation))}
                        <CommonIconButton
                          icon="external"
                          label={t("InstanceSyncPage.platform.openOriginal")}
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
                        label={t("InstanceSyncPage.platform.openOriginal")}
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
                items={[
                  <HStack
                    key="head"
                    spacing={2}
                    justify="space-between"
                    align="start"
                    p={0.5}
                  >
                    <VStack spacing={0} align="stretch" flex={1} minW={0}>
                      <Text fontSize="sm" fontWeight="bold" noOfLines={1}>
                        {project.name}
                      </Text>
                      {project.summary && (
                        <Text
                          fontSize="xs"
                          className="secondary-text"
                          noOfLines={1}
                        >
                          {project.summary}
                        </Text>
                      )}
                    </VStack>
                    {deleteButton(project)}
                  </HStack>,
                  ...project.lines.map((line) => ({
                    title: describeLine(line),
                    description: changelogOf(line) && (
                      <Tooltip
                        label={changelogOf(line)}
                        placement="bottom-start"
                        whiteSpace="pre-wrap"
                        openDelay={300}
                      >
                        <Text
                          fontSize="xs"
                          className="secondary-text"
                          noOfLines={2}
                          whiteSpace="pre-wrap"
                          wordBreak="break-all"
                        >
                          {changelogOf(line)}
                        </Text>
                      </Tooltip>
                    ),
                    children: installButton(project, line),
                  })),
                ]}
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

export default CampusPlatformSection;
