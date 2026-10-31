import { Box } from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import CampusPlatformSection from "@/components/campus-platform-section";
import { Section } from "@/components/common/section";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import {
  CampusModpackProject,
  CampusModpackRecommendation,
} from "@/models/campus-modpack";
import { CampusModpackService } from "@/services/campus-modpack";

const DiscoverCampusModpacksPage = () => {
  const { t } = useTranslation();
  const router = useRouter();
  const toast = useToast();
  const { getInstanceList } = useGlobalData();
  const instances = useMemo(() => getInstanceList() ?? [], [getInstanceList]);

  const [projects, setProjects] = useState<CampusModpackProject[]>([]);
  const [recommendation, setRecommendation] =
    useState<CampusModpackRecommendation | null>(null);
  const [loading, setLoading] = useState(true);

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

  const focusInstance = useCallback(
    (instanceId: string) => {
      void router.push(`/instances/details/${encodeURIComponent(instanceId)}`);
    },
    [router]
  );

  return (
    <Section
      title={t("DiscoverCampusModpacksPage.title")}
      description={t("DiscoverCampusModpacksPage.description")}
      w="100%"
      h="100%"
      display="flex"
      flexDir="column"
    >
      <Box overflow="auto" flexGrow={1}>
        <CampusPlatformSection
          projects={projects}
          recommendation={recommendation}
          loading={loading}
          onRefresh={fetchAll}
          instances={instances}
          onFocusInstance={focusInstance}
        />
      </Box>
    </Section>
  );
};

export default DiscoverCampusModpacksPage;
