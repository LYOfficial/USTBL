import {
  Button,
  HStack,
  Select,
  Text,
  Textarea,
  VStack,
} from "@chakra-ui/react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { OptionItemGroup } from "@/components/common/option-item";
import { useLauncherConfig } from "@/contexts/config";
import { useToast } from "@/contexts/toast";
import { InvokeResponse } from "@/models/response";
import {
  Ipv6AccelerationService,
  Ipv6Node,
  Ipv6Status,
} from "@/services/ipv6-acceleration";

export default function Ipv6AccelerationSettings() {
  const { t } = useTranslation();
  const toast = useToast();
  const { config } = useLauncherConfig();
  const [status, setStatus] = useState<Ipv6Status>();
  const [busy, setBusy] = useState(false);
  const [nodes, setNodes] = useState<Ipv6Node[]>([]);
  const [bypass, setBypass] = useState("");
  const [error, setError] = useState("");
  const mounted = useRef(false);
  const pending = useRef(false);
  const key = "ResourceAccelerationSettingsPage.ipv6";

  const refresh = useCallback(async () => {
    if (pending.current) return;
    const response = await Ipv6AccelerationService.status();
    if (!mounted.current || pending.current) return;
    if (response.status === "success") {
      setStatus(response.data);
      setError(response.data.error || "");
    } else setError(response.details || response.message);
  }, []);

  useEffect(() => {
    mounted.current = true;
    Ipv6AccelerationService.status().then((response) => {
      if (!mounted.current) return;
      if (response.status === "success") {
        setStatus(response.data);
        setBypass(response.data.settings.bypass.join("\n"));
        setError(response.data.error || "");
      } else setError(response.details || response.message);
    });
    const timer = window.setInterval(refresh, 3000);
    return () => {
      mounted.current = false;
      window.clearInterval(timer);
    };
  }, [refresh]);

  const run = async <T,>(
    action: () => Promise<InvokeResponse<T>>,
    success: (data: T) => void
  ) => {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    setError("");
    try {
      const response = await action();
      if (!mounted.current) return;
      if (response.status === "success") success(response.data);
      else {
        setError(response.details || response.message);
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
    } finally {
      pending.current = false;
      if (mounted.current) setBusy(false);
    }
  };

  const acceptStatus = (data: Ipv6Status) => {
    setStatus(data);
    setNodes([]);
    setError(data.error || "");
  };
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
                  onClick={() =>
                    run(Ipv6AccelerationService.install, acceptStatus)
                  }
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
                onClick={() =>
                  run(
                    () =>
                      status?.running
                        ? Ipv6AccelerationService.stop()
                        : Ipv6AccelerationService.start({
                            bypass: bypass
                              .split(/\r?\n/)
                              .filter((line) => line.trim()),
                          }),
                    acceptStatus
                  )
                }
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
                onChange={(event) =>
                  run(
                    () => Ipv6AccelerationService.select(event.target.value),
                    () =>
                      setNodes((items) =>
                        items.map((node) => ({
                          ...node,
                          selected: node.name === event.target.value,
                        }))
                      )
                  )
                }
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
                  onClick={() =>
                    run(() => Ipv6AccelerationService.nodes(), setNodes)
                  }
                >
                  {t(`${key}.refresh`)}
                </Button>
                <Button
                  size="xs"
                  variant="outline"
                  isDisabled={!status?.running || busy}
                  onClick={() =>
                    run(() => Ipv6AccelerationService.nodes(true), setNodes)
                  }
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
