import {
  Button,
  HStack,
  Select,
  Text,
  Textarea,
  VStack,
} from "@chakra-ui/react";
import { useTranslation } from "react-i18next";
import { OptionItemGroup } from "@/components/common/option-item";
import { useLauncherConfig } from "@/contexts/config";
import { useIpv6Acceleration } from "@/hooks/ipv6-acceleration";

export default function Ipv6AccelerationSettings() {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const {
    status,
    busy,
    nodes,
    bypass,
    setBypass,
    error,
    toggle,
    install,
    refreshNodes,
    selectNode,
  } = useIpv6Acceleration();
  const key = "ResourceAccelerationSettingsPage.ipv6";
  const selected = nodes.find((node) => node.selected)?.name || "Auto";
  return (
    <OptionItemGroup
      title={t(`${key}.title`)}
      items={[
        {
          title: t(`${key}.current`),
          description: (
            <VStack align="stretch" spacing={1}>
              <Text fontSize="xs">{t(`${key}.description`)}</Text>
              <Text fontSize="xs">
                {t(`${key}.${status?.running ? "running" : "stopped"}`)}
              </Text>
              {status && !status.supported && (
                <Text fontSize="xs" color="orange.500">
                  {t(`${key}.unsupported`)}
                </Text>
              )}
              {status?.supported && !status.elevated && (
                <Text fontSize="xs" color="orange.500">
                  {t(`${key}.administrator`)}
                </Text>
              )}
              {busy && <Text fontSize="xs">{t(`${key}.working`)}</Text>}
              {error && (
                <Text fontSize="xs" color="red.500" overflowWrap="anywhere">
                  {error}
                </Text>
              )}
            </VStack>
          ),
          children: (
            <HStack>
              {status && !status.running && (
                <Button
                  size="xs"
                  isLoading={busy}
                  isDisabled={!status.supported || busy}
                  onClick={install}
                >
                  {t(`${key}.install`)}
                </Button>
              )}
              <Button
                size="xs"
                colorScheme={config.appearance.theme.primaryColor}
                isLoading={busy}
                isDisabled={
                  !status ||
                  !status.supported ||
                  busy ||
                  (!status.running && (!status.installed || !status.elevated))
                }
                onClick={toggle}
              >
                {t(`${key}.${status?.running ? "stop" : "start"}`)}
              </Button>
            </HStack>
          ),
        },
        {
          title: t(`${key}.bypass`),
          description: t(`${key}.bypassDescription`),
          children: (
            <Textarea
              size="xs"
              width="230px"
              minHeight="72px"
              maxLength={8192}
              value={bypass}
              aria-label={t(`${key}.bypass`)}
              isDisabled={!status || status.running || busy}
              onChange={(event) => setBypass(event.target.value)}
            />
          ),
        },
        {
          title: t(`${key}.nodes`),
          description: (
            <Text fontSize="xs">{t(`${key}.nodesDescription`)}</Text>
          ),
          children: (
            <VStack align="stretch" width="230px">
              <Select
                size="xs"
                value={selected}
                aria-label={t(`${key}.nodes`)}
                isDisabled={!status?.running || busy}
                onChange={(event) => selectNode(event.target.value)}
              >
                {nodes.length === 0 && (
                  <option value="Auto">{t(`${key}.auto`)}</option>
                )}
                {nodes.map((node) => (
                  <option key={node.name} value={node.name}>
                    {node.name === "Auto" ? t(`${key}.auto`) : node.name}{" "}
                    {node.latencyMs === null ? "" : `${node.latencyMs} ms`}
                  </option>
                ))}
              </Select>
              <HStack>
                <Button
                  size="xs"
                  variant="outline"
                  isDisabled={!status?.running || busy}
                  onClick={() => refreshNodes()}
                >
                  {t(`${key}.refresh`)}
                </Button>
                <Button
                  size="xs"
                  variant="outline"
                  isDisabled={!status?.running || busy}
                  onClick={() => refreshNodes(true)}
                >
                  {t(`${key}.test`)}
                </Button>
              </HStack>
            </VStack>
          ),
        },
        {
          title: t(`${key}.notice`),
          description: t(`${key}.noticeDescription`),
          children: <></>,
        },
      ]}
    />
  );
}
