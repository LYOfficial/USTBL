import {
  Button,
  Center,
  HStack,
  Spinner,
  Switch,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useEffect, useState } from "react";
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
import { useSharedModals } from "@/contexts/shared-modal";
import { useResourceAcceleration } from "@/hooks/resource-acceleration";
import { AccountService } from "@/services/account";

const ResourceAccelerationSettingsPage = () => {
  const { t } = useTranslation();
  const router = useRouter();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const { requireVustbAccount } = useSharedModals();
  const [accountState, setAccountState] = useState<
    "loading" | "authenticated" | "unauthenticated"
  >("loading");
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

  useEffect(() => {
    let disposed = false;
    void AccountService.retrieveVustbAccount().then((response) => {
      if (disposed) return;
      setAccountState(
        response.status === "success" && response.data
          ? "authenticated"
          : "unauthenticated"
      );
    });
    return () => {
      disposed = true;
    };
  }, []);

  if (accountState === "loading") {
    return (
      <Center minH="240px">
        <Spinner />
      </Center>
    );
  }

  if (accountState === "unauthenticated") {
    return (
      <Center minH="240px" px={6}>
        <VStack spacing={4} maxW="520px" textAlign="center">
          <Text>{t("VustbAccessGate.description")}</Text>
          <Button
            colorScheme={primaryColor}
            onClick={() => void router.push("/accounts")}
          >
            {t("VustbAccessGate.goToAccounts")}
          </Button>
        </VStack>
      </Center>
    );
  }

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
              onChange={(event) =>
                requireVustbAccount(() =>
                  setGithubEnabled(event.target.checked)
                )
              }
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
                onClick={() => requireVustbAccount(toggle)}
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
              onClick={() => requireVustbAccount(testLatency)}
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
