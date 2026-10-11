import { Button, HStack, Switch, Text } from "@chakra-ui/react";
import { useTranslation } from "react-i18next";
import {
  OptionItemGroup,
  OptionItemGroupProps,
} from "@/components/common/option-item";
import {
  AccelerationLatencyView,
  AccelerationProgressView,
} from "@/components/settings/acceleration-feedback";
import Ipv6AccelerationSettings from "@/components/settings/ipv6-acceleration";
import { useLauncherConfig } from "@/contexts/config";
import { useResourceAcceleration } from "@/hooks/resource-acceleration";

const ResourceAccelerationSettingsPage = () => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const {
    githubEnabled,
    setGithubEnabled,
    status,
    loading,
    progress,
    elapsed,
    latencyLoading,
    latencies,
    toggle,
    testLatency,
  } = useResourceAcceleration();

  const groups: OptionItemGroupProps[] = [
    {
      title: t("ResourceAccelerationSettingsPage.services.title"),
      items: [
        {
          title: t("ResourceAccelerationSettingsPage.services.github.title"),
          description: t(
            "ResourceAccelerationSettingsPage.services.github.description"
          ),
          children: (
            <Switch
              colorScheme={primaryColor}
              isChecked={githubEnabled}
              isDisabled={status.running || loading}
              onChange={(event) => setGithubEnabled(event.target.checked)}
            />
          ),
        },
      ],
    },
    {
      title: t("ResourceAccelerationSettingsPage.status.title"),
      items: [
        {
          title: t("ResourceAccelerationSettingsPage.status.current"),
          description: status.running
            ? t("ResourceAccelerationSettingsPage.status.running", {
                elapsed: formatElapsed(elapsed),
              })
            : t("ResourceAccelerationSettingsPage.status.stopped"),
          children: (
            <HStack>
              <Button
                size="xs"
                colorScheme={primaryColor}
                isLoading={loading}
                onClick={toggle}
                isDisabled={
                  loading ||
                  latencyLoading ||
                  (!status.running && !githubEnabled)
                }
              >
                {t(
                  status.running
                    ? "ResourceAccelerationSettingsPage.actions.stop"
                    : "ResourceAccelerationSettingsPage.actions.start"
                )}
              </Button>
            </HStack>
          ),
        },
        ...(loading
          ? [
              {
                title: t("ResourceAccelerationSettingsPage.progress.title"),
                description: (
                  <AccelerationProgressView
                    progress={progress}
                    colorScheme={primaryColor}
                  />
                ),
                children: <></>,
              },
            ]
          : []),
        {
          title: t("ResourceAccelerationSettingsPage.latency.title"),
          description: (
            <AccelerationLatencyView
              running={status.running}
              latencies={latencies}
            />
          ),
          children: (
            <Button
              size="xs"
              variant="outline"
              isLoading={latencyLoading}
              isDisabled={loading}
              onClick={testLatency}
            >
              {t("ResourceAccelerationSettingsPage.latency.test")}
            </Button>
          ),
        },
        {
          title: t("ResourceAccelerationSettingsPage.notice.title"),
          description: (
            <Text fontSize="xs" className="secondary-text">
              {t("ResourceAccelerationSettingsPage.notice.description")}
            </Text>
          ),
          children: <></>,
        },
      ],
    },
  ];

  return (
    <>
      {groups.map((group, index) => (
        <OptionItemGroup {...group} key={index} />
      ))}
      <Ipv6AccelerationSettings />
    </>
  );
};

function formatElapsed(seconds: number) {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const remaining = seconds % 60;
  return [hours, minutes, remaining]
    .map((value) => String(value).padStart(2, "0"))
    .join(":");
}

export default ResourceAccelerationSettingsPage;
