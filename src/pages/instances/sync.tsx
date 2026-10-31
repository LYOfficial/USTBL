import { Box, VStack } from "@chakra-ui/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import CampusPlatformSection from "@/components/campus-platform-section";
import { Section } from "@/components/common/section";
import { InstanceLedger, ledgerRowId } from "@/components/sync/instance-ledger";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import {
  CampusModpackProject,
  CampusModpackRecommendation,
} from "@/models/campus-modpack";
import { CampusModpackService } from "@/services/campus-modpack";

const HIGHLIGHT_MS = 1600;

const InstanceSyncPage = () => {
  const { t } = useTranslation();
  const toast = useToast();
  const { getInstanceList } = useGlobalData();
  const instances = useMemo(() => getInstanceList() ?? [], [getInstanceList]);

  const [projects, setProjects] = useState<CampusModpackProject[]>([]);
  const [recommendation, setRecommendation] =
    useState<CampusModpackRecommendation | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [highlightId, setHighlightId] = useState<string | null>(null);
  const highlightTimer = useRef<ReturnType<typeof setTimeout>>();

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

  useEffect(() => () => clearTimeout(highlightTimer.current), []);

  const focusInstance = useCallback((instanceId: string) => {
    document
      .getElementById(ledgerRowId(instanceId))
      ?.scrollIntoView({ behavior: "smooth", block: "center" });
    setHighlightId(instanceId);
    clearTimeout(highlightTimer.current);
    highlightTimer.current = setTimeout(
      () => setHighlightId(null),
      HIGHLIGHT_MS
    );
  }, []);

  return (
    <Section
      title={t("InstanceSyncPage.title")}
      description={t("InstanceSyncPage.description")}
      w="100%"
      h="100%"
      display="flex"
      flexDir="column"
    >
      <Box overflow="auto" flexGrow={1}>
        <VStack align="stretch" spacing={5}>
          <InstanceLedger instances={instances} highlightId={highlightId} />
          <CampusPlatformSection
            projects={projects}
            recommendation={recommendation}
            loading={loading}
            onRefresh={fetchAll}
            instances={instances}
            onFocusInstance={focusInstance}
          />
        </VStack>
      </Box>
    </Section>
  );
};

export default InstanceSyncPage;
