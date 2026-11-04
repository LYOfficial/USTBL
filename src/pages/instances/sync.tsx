import { Box } from "@chakra-ui/react";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { Section } from "@/components/common/section";
import { InstanceLedger } from "@/components/sync/instance-ledger";
import { useGlobalData } from "@/contexts/global-data";

const InstanceSyncPage = () => {
  const { t } = useTranslation();
  const { getInstanceList } = useGlobalData();
  const instances = useMemo(() => getInstanceList() ?? [], [getInstanceList]);

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
        <InstanceLedger instances={instances} />
      </Box>
    </Section>
  );
};

export default InstanceSyncPage;
