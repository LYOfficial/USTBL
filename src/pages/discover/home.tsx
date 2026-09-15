import {
  Avatar,
  Badge,
  Box,
  Card,
  HStack,
  SimpleGrid,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { BeatLoader } from "react-spinners";
import { CommonIconButton } from "@/components/common/common-icon-button";
import Empty from "@/components/common/empty";
import { Section } from "@/components/common/section";
import { useToast } from "@/contexts/toast";
import { McServerStatus } from "@/models/mc-server";
import { DiscoverService } from "@/services/discover";
import { copyText } from "@/utils/copy";

const serverMotd = (server: McServerStatus) =>
  server.motdSegments
    .map((segment) => segment.text)
    .join("")
    .trim();

const serverVersion = (server: McServerStatus) =>
  server.versionHint || server.version || "-";

export const DiscoverHomePage = () => {
  const { t } = useTranslation();
  const toast = useToast();
  const [servers, setServers] = useState<McServerStatus[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState("");

  const fetchServers = useCallback(async () => {
    setIsLoading(true);
    setError("");
    try {
      const response = await DiscoverService.fetchVustbServerStatuses();
      if (response.status === "success") {
        setServers(response.data);
      } else {
        setError(response.details || response.message);
      }
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchServers();
  }, [fetchServers]);

  return (
    <Section px={{ base: 4, lg: 6 }}>
      <VStack align="stretch" spacing={4} pb={4}>
        <HStack justify="space-between">
          <Box>
            <Text fontSize="lg" fontWeight="semibold">
              {t("DiscoverHomePage.serverList.title")}
            </Text>
            <Text fontSize="sm" className="secondary-text">
              {t("DiscoverHomePage.serverList.description")}
            </Text>
          </Box>
          <CommonIconButton
            icon="refresh"
            onClick={fetchServers}
            isDisabled={isLoading}
          />
        </HStack>

        {isLoading && servers.length === 0 ? (
          <VStack py={10}>
            <BeatLoader size={14} color="gray" />
          </VStack>
        ) : error && servers.length === 0 ? (
          <VStack py={10} spacing={3}>
            <Text color="red.400" fontSize="sm">
              {error}
            </Text>
            <CommonIconButton
              icon="refresh"
              label={t("DiscoverHomePage.serverList.retry")}
              onClick={fetchServers}
            />
          </VStack>
        ) : servers.length === 0 ? (
          <Empty withIcon={false} size="sm" />
        ) : (
          <SimpleGrid columns={{ base: 1, xl: 2 }} gap={4}>
            {servers.map((server) => {
              const online = server.serverStatus === "online";
              const address =
                server.exposeIp && server.address ? server.address : null;
              const motd = serverMotd(server);

              return (
                <Card key={server.id} p={4}>
                  <VStack align="stretch" spacing={3}>
                    <HStack align="start" justify="space-between">
                      <HStack minW={0} align="start">
                        <Avatar
                          name={server.name}
                          src={server.icon || server.iconUrl || undefined}
                          boxSize="48px"
                          borderRadius="sm"
                        />
                        <Box minW={0}>
                          <HStack spacing={2} flexWrap="wrap">
                            <Text fontWeight="semibold">{server.name}</Text>
                            <Badge colorScheme={online ? "green" : "red"}>
                              {t(
                                `DiscoverHomePage.serverList.status.${
                                  online ? "online" : "offline"
                                }`
                              )}
                            </Badge>
                          </HStack>
                          <Text
                            fontSize="sm"
                            className="secondary-text"
                            noOfLines={2}
                          >
                            {server.description || server.theme || "-"}
                          </Text>
                        </Box>
                      </HStack>
                      <Text fontSize="sm" whiteSpace="nowrap">
                        {server.playersOnline ?? 0} / {server.playersMax ?? 0}
                      </Text>
                    </HStack>

                    {motd && (
                      <Text fontSize="sm" whiteSpace="pre-line" noOfLines={2}>
                        {motd}
                      </Text>
                    )}

                    <HStack justify="space-between" align="end" spacing={3}>
                      <Box minW={0}>
                        <Text fontSize="xs" className="secondary-text">
                          {t("DiscoverHomePage.serverList.version")}:{" "}
                          {serverVersion(server)}
                        </Text>
                        <Text
                          fontSize="sm"
                          fontFamily="mono"
                          className="ellipsis-text"
                        >
                          {address ||
                            t("DiscoverHomePage.serverList.hiddenAddress")}
                        </Text>
                      </Box>
                      {address && (
                        <CommonIconButton
                          icon="copy"
                          label={t("DiscoverHomePage.serverList.copyAddress")}
                          onClick={() => copyText(address, { toast })}
                          flexShrink={0}
                        />
                      )}
                    </HStack>
                  </VStack>
                </Card>
              );
            })}
          </SimpleGrid>
        )}
      </VStack>
    </Section>
  );
};

export default DiscoverHomePage;
