import {
  HStack,
  Progress,
  Tag,
  TagLabel,
  Text,
  Tooltip,
  VStack,
} from "@chakra-ui/react";
import { useTranslation } from "react-i18next";
import { AccelerationProgress } from "@/hooks/resource-acceleration";
import { ResourceAccelerationLatency } from "@/services/resource-acceleration";

export function AccelerationProgressView({
  progress,
  colorScheme,
}: {
  progress: AccelerationProgress;
  colorScheme: string;
}) {
  const { t } = useTranslation();
  return (
    <VStack align="stretch" spacing={1} width="100%" alignSelf="stretch">
      <Text fontSize="xs" className="secondary-text">
        {progress.message ||
          t("ResourceAccelerationSettingsPage.progress.loading")}
      </Text>
      <Progress
        width="100%"
        value={
          progress.total
            ? (progress.completed / progress.total) * 100
            : undefined
        }
        size="xs"
        colorScheme={colorScheme}
        isIndeterminate={!progress.total}
      />
      <Text fontSize="xs" className="secondary-text">
        {progress.completed}/{progress.total || "?"}
      </Text>
    </VStack>
  );
}

export function AccelerationLatencyView({
  running,
  latencies,
}: {
  running: boolean;
  latencies: ResourceAccelerationLatency[];
}) {
  const { t } = useTranslation();
  const error = latencies.find((item) => item.error)?.error;
  return (
    <VStack align="stretch" spacing={1} width="100%">
      <Text fontSize="xs" className="secondary-text">
        {t(
          `ResourceAccelerationSettingsPage.latency.${running ? "proxy" : "direct"}`
        )}
      </Text>
      {error && (
        <Text fontSize="xs" color="red.500" overflowWrap="anywhere">
          {error}
        </Text>
      )}
      {latencies.length === 0 ? (
        <Text fontSize="xs" className="secondary-text">
          {t("ResourceAccelerationSettingsPage.latency.empty")}
        </Text>
      ) : (
        latencies.map((item) => (
          <HStack key={item.host} justify="space-between" width="100%">
            <Text fontSize="xs">{item.host}</Text>
            <Tooltip label={item.error} isDisabled={!item.error}>
              <Tag
                size="sm"
                colorScheme={
                  !item.available
                    ? "red"
                    : (item.latencyMs ?? 0) < 200
                      ? "green"
                      : (item.latencyMs ?? 0) < 800
                        ? "yellow"
                        : "orange"
                }
              >
                <TagLabel>
                  {item.available
                    ? `${item.latencyMs} ms`
                    : t("ResourceAccelerationSettingsPage.latency.failed")}
                </TagLabel>
              </Tag>
            </Tooltip>
          </HStack>
        ))
      )}
    </VStack>
  );
}
