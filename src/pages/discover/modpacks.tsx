import { Box } from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useTranslation } from "react-i18next";
import { Section } from "@/components/common/section";
import ResourceDownloader from "@/components/resource-downloader";
import { OtherResourceSource, OtherResourceType } from "@/enums/resource";

const validSources = new Set(Object.values(OtherResourceSource));

const DiscoverModpacksPage = () => {
  const { t } = useTranslation();
  const router = useRouter();

  if (!router.isReady) return null;

  const query = typeof router.query.q === "string" ? router.query.q : "";
  const sourceParam =
    typeof router.query.source === "string" ? router.query.source : "";
  const source = validSources.has(sourceParam as OtherResourceSource)
    ? (sourceParam as OtherResourceSource)
    : OtherResourceSource.Modrinth;

  return (
    <Section
      title={t("DiscoverModpacksPage.title")}
      description={t("DiscoverModpacksPage.description")}
      w="100%"
      h="100%"
      display="flex"
      flexDir="column"
    >
      <Box
        flex="1"
        bg="chakra-body-bg"
        borderWidth="1px"
        borderRadius="lg"
        boxShadow="sm"
        pt={2}
        px={1}
      >
        <ResourceDownloader
          resourceType={OtherResourceType.ModPack}
          initialSearchQuery={query}
          initialDownloadSource={source}
          displayInModal={false}
        />
      </Box>
    </Section>
  );
};

export default DiscoverModpacksPage;
