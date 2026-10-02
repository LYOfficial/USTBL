import {
  Button,
  HStack,
  Progress,
  Switch,
  Tag,
  TagLabel,
  Text,
  VStack,
} from "@chakra-ui/react";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  OptionItemGroup,
  OptionItemGroupProps,
} from "@/components/common/option-item";
import { useLauncherConfig } from "@/contexts/config";
import { useToast } from "@/contexts/toast";
import {
  ResourceAccelerationLatency,
  ResourceAccelerationService,
  ResourceAccelerationStatus,
} from "@/services/resource-acceleration";

const ResourceAccelerationSettingsPage = () => {
  const { t } = useTranslation();
  const toast = useToast();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const [githubEnabled, setGithubEnabled] = useState(true);
  const [status, setStatus] = useState<ResourceAccelerationStatus>({
    running: false,
    loading: false,
    githubEnabled: false,
    hostsTotal: 0,
    hostsCompleted: 0,
    startedAt: null,
  });
  const [loading, setLoading] = useState(false);
  const [progress, setProgress] = useState({
    completed: 0,
    total: 0,
    message: "",
  });
  const [elapsed, setElapsed] = useState(0);
  const [latencyLoading, setLatencyLoading] = useState(false);
  const [latencies, setLatencies] = useState<ResourceAccelerationLatency[]>([]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<{ completed: number; total: number; message: string }>(
      "resource-acceleration-progress",
      (event) => setProgress(event.payload)
    ).then((cleanup) => {
      unlisten = cleanup;
    });
    ResourceAccelerationService.status().then((response) => {
      if (response.status === "success") {
        setStatus(response.data);
        setGithubEnabled(response.data.githubEnabled);
      }
    });
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    if (!status.running || !status.startedAt) {
      setElapsed(0);
      return;
    }
    const update = () =>
      setElapsed(
        Math.max(0, Math.floor((Date.now() - status.startedAt!) / 1000))
      );
    update();
    const timer = window.setInterval(update, 1000);
    return () => window.clearInterval(timer);
  }, [status.running, status.startedAt]);

  const handleStart = async () => {
    setLoading(true);
    const response = await ResourceAccelerationService.start(
      githubEnabled
    );
    setLoading(false);
    if (response.status === "success") {
      setStatus(response.data);
      setProgress({ completed: 0, total: 0, message: "" });
      toast({
        title: t("ResourceAccelerationSettingsPage.toast.started"),
        status: "success",
      });
    } else
      toast({
        title: response.message,
        description: response.details,
        status: "error",
      });
  };

  const handleStop = async () => {
    setLoading(true);
    const response = await ResourceAccelerationService.stop();
    setLoading(false);
    if (response.status === "success") {
      setStatus(response.data);
      setProgress({ completed: 0, total: 0, message: "" });
      toast({
        title: t("ResourceAccelerationSettingsPage.toast.stopped"),
        status: "success",
      });
    } else
      toast({
        title: response.message,
        description: response.details,
        status: "error",
      });
  };

  const handleLatencyTest = async () => {
    setLatencyLoading(true);
    const response = await ResourceAccelerationService.testLatency(
      status.running ? status.githubEnabled : githubEnabled
    );
    setLatencyLoading(false);
    if (response.status === "success") {
      setLatencies(response.data);
    } else {
      toast({
        title: response.message,
        description: response.details,
        status: "error",
      });
    }
  };

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
                onClick={status.running ? handleStop : handleStart}
                isDisabled={
                  loading ||
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
                  <VStack
                    align="stretch"
                    spacing={1}
                    width="100%"
                    alignSelf="stretch"
                  >
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
                      colorScheme={primaryColor}
                      isIndeterminate={!progress.total}
                    />
                    <Text fontSize="xs" className="secondary-text">
                      {progress.completed}/{progress.total || "?"}
                    </Text>
                  </VStack>
                ),
                children: <></>,
              },
            ]
          : []),
        {
          title: t("ResourceAccelerationSettingsPage.latency.title"),
          description: (
            <VStack align="stretch" spacing={1} width="100%">
              {latencies.length === 0 ? (
                <Text fontSize="xs" className="secondary-text">
                  {t("ResourceAccelerationSettingsPage.latency.empty")}
                </Text>
              ) : (
                latencies.map((item) => (
                  <HStack key={item.host} justify="space-between" width="100%">
                    <Text fontSize="xs">{item.host}</Text>
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
                          : t(
                              "ResourceAccelerationSettingsPage.latency.failed"
                            )}
                      </TagLabel>
                    </Tag>
                  </HStack>
                ))
              )}
            </VStack>
          ),
          children: (
            <Button
              size="xs"
              variant="outline"
              isLoading={latencyLoading}
              isDisabled={loading}
              onClick={handleLatencyTest}
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
