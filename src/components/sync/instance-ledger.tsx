import { Badge, Button, HStack, Text, VStack } from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { CommonIconButton } from "@/components/common/common-icon-button";
import { OptionItemGroup } from "@/components/common/option-item";
import { Section } from "@/components/common/section";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useSharedModals } from "@/contexts/shared-modal";
import { ModLoaderType } from "@/enums/instance";
import { useLocalModpackUpdate } from "@/hooks/local-modpack-update";
import { InstanceSummary } from "@/models/instance/misc";
import {
  isInstallingInstance,
  shouldCheckInstance,
} from "@/utils/installing-instance";
import {
  LedgerBadge,
  ledgerBadge,
  ledgerInstances,
  ledgerLocalChangesDisabled,
  ledgerLocalInstances,
  ledgerScanned,
  ledgerUpdateDisabled,
  loaderSlug,
} from "@/utils/modpack-ledger";

const BADGE_COLORS: Record<LedgerBadge, string> = {
  upToDate: "green",
  available: "blue",
  paused: "gray",
  needsRepair: "red",
  waitingLoader: "yellow",
  installing: "yellow",
  unknown: "gray",
  notChecked: "gray",
  unlinked: "gray",
};

interface InstanceLedgerProps {
  instances: InstanceSummary[];
}

/**
 * Every instance that follows a modpack line, with where it stands against
 * that line. Instances without an origin are not listed.
 */
export const InstanceLedger: React.FC<InstanceLedgerProps> = ({
  instances,
}) => {
  const { t } = useTranslation();
  const router = useRouter();
  const { openSharedModal } = useSharedModals();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const { modpackChecks, checkModpackUpdate } = useGlobalData();

  const [checking, setChecking] = useState(false);

  const { updateFromFile, pickingId } = useLocalModpackUpdate();

  const rows = useMemo(() => ledgerInstances(instances), [instances]);
  const localRows = useMemo(() => ledgerLocalInstances(instances), [instances]);

  const followedKey = rows
    .filter(shouldCheckInstance)
    .map((instance) => instance.id)
    .join("\n");
  // Local imports are scanned too, for the change count behind "view changes".
  const localKey = localRows
    .filter((instance) => !isInstallingInstance(instance))
    .map((instance) => instance.id)
    .join("\n");

  // One at a time, so a slow platform does not pile up requests. This is the
  // scan that finds local edits, which the quick startup check skips.
  const checkAll = useCallback(
    async (isCancelled: () => boolean) => {
      setChecking(true);
      const ids = [followedKey, localKey].flatMap((key) =>
        key ? key.split("\n") : []
      );
      for (const id of ids) {
        if (isCancelled()) break;
        await checkModpackUpdate(id, true);
      }
      setChecking(false);
    },
    [followedKey, localKey, checkModpackUpdate]
  );

  useEffect(() => {
    let cancelled = false;
    checkAll(() => cancelled);
    return () => {
      cancelled = true;
    };
  }, [checkAll]);

  const describeLoader = useCallback(
    (loader: ModLoaderType) =>
      loaderSlug(loader) === "vanilla"
        ? t("InstanceSyncPage.platform.vanilla")
        : loader,
    [t]
  );

  const lineLabel = (instance: InstanceSummary): string => {
    const origin = instance.origin;
    if (!origin || origin.platform === "local") return instance.version;
    const platformName =
      origin.platform === "campus"
        ? t("InstanceSyncPage.ledger.platform.campus")
        : origin.platform === "modrinth"
          ? "Modrinth"
          : "CurseForge";
    return [
      platformName,
      origin.gameVersion,
      describeLoader(origin.loaderType),
    ].join(" · ");
  };

  if (rows.length === 0 && localRows.length === 0) return null;

  return (
    <VStack align="stretch" spacing={3}>
      {rows.length > 0 && (
        <Section
          title={t("InstanceSyncPage.ledger.group.platform")}
          isAccordion
          titleExtra={
            <CommonIconButton
              icon="external"
              label={t("InstanceSyncPage.ledger.installMore")}
              withTooltip
              onClick={() => router.push("/discover/modpacks")}
              size="xs"
              h={21}
            />
          }
          headExtra={
            <CommonIconButton
              icon="refresh"
              label={t("InstanceSyncPage.ledger.checkAll")}
              withTooltip
              onClick={() => checkAll(() => false)}
              isDisabled={checking || (!followedKey && !localKey)}
              size="xs"
              h={21}
            />
          }
        >
          <OptionItemGroup
            items={rows.map((instance) => {
              const check = modpackChecks[instance.id];
              const badge = ledgerBadge(instance, check);
              const openDetails = () =>
                openSharedModal("modpack-instance-details", {
                  instanceId: instance.id,
                  instanceName: instance.name,
                });
              const openUpdate = () =>
                openSharedModal("update-modpack", {
                  instanceId: instance.id,
                  instanceName: instance.name,
                });
              const openChanges = () =>
                openSharedModal("modpack-local-changes", {
                  instanceId: instance.id,
                  instanceName: instance.name,
                });

              const detail =
                badge === "available" && check?.latestLabel
                  ? t("InstanceSyncPage.ledger.updateTo", {
                      latest: check.latestLabel,
                    })
                  : (badge === "needsRepair" || badge === "unknown") &&
                      check?.reason
                    ? t(`UpdateModpackModal.reason.${check.reason}`, {
                        defaultValue: check.reason,
                      })
                    : null;

              const actions = (
                <HStack spacing={1}>
                  <Button size="xs" variant="outline" onClick={openDetails}>
                    {t("InstanceSyncPage.ledger.action.details")}
                  </Button>
                  <Button
                    size="xs"
                    variant="outline"
                    isDisabled={!ledgerScanned(check)}
                    onClick={openChanges}
                  >
                    {t("InstanceSyncPage.ledger.action.viewChanges")}
                  </Button>
                  <Button
                    size="xs"
                    colorScheme={primaryColor}
                    onClick={openUpdate}
                    isDisabled={ledgerUpdateDisabled(instance, check)}
                  >
                    {t("InstanceSyncPage.ledger.action.update")}
                  </Button>
                </HStack>
              );

              return {
                title: (
                  <HStack spacing={2}>
                    <Text fontSize="xs-sm">{instance.name}</Text>
                    <Badge colorScheme={BADGE_COLORS[badge]}>
                      {t(`InstanceSyncPage.ledger.badge.${badge}`)}
                    </Badge>
                  </HStack>
                ),
                description: (
                  <VStack spacing={0} align="stretch">
                    <Text fontSize="xs" className="secondary-text">
                      {lineLabel(instance)}
                    </Text>
                    <Text fontSize="xs" className="secondary-text">
                      {[check?.currentLabel ?? instance.modpackVersion, detail]
                        .filter(Boolean)
                        .join(" · ")}
                    </Text>
                  </VStack>
                ),
                children: actions,
              };
            })}
          />
        </Section>
      )}
      {localRows.length > 0 && (
        <Section title={t("InstanceSyncPage.ledger.group.local")} isAccordion>
          <OptionItemGroup
            items={localRows.map((instance) => {
              const installing = isInstallingInstance(instance);
              const check = modpackChecks[instance.id];
              return {
                title: (
                  <HStack spacing={2}>
                    <Text fontSize="xs-sm">{instance.name}</Text>
                    {installing && (
                      <Badge colorScheme={BADGE_COLORS.installing}>
                        {t("InstanceSyncPage.ledger.badge.installing")}
                      </Badge>
                    )}
                  </HStack>
                ),
                description: (
                  <VStack spacing={0} align="stretch">
                    <Text fontSize="xs" className="secondary-text">
                      {lineLabel(instance)}
                    </Text>
                    {instance.modpackVersion && (
                      <Text fontSize="xs" className="secondary-text">
                        {instance.modpackVersion}
                      </Text>
                    )}
                  </VStack>
                ),
                children: (
                  <HStack spacing={1}>
                    <Button
                      size="xs"
                      variant="outline"
                      onClick={() =>
                        openSharedModal("modpack-instance-details", {
                          instanceId: instance.id,
                          instanceName: instance.name,
                        })
                      }
                    >
                      {t("InstanceSyncPage.ledger.action.details")}
                    </Button>
                    <Button
                      size="xs"
                      variant="outline"
                      isDisabled={ledgerLocalChangesDisabled(instance, check)}
                      onClick={() =>
                        openSharedModal("modpack-local-changes", {
                          instanceId: instance.id,
                          instanceName: instance.name,
                        })
                      }
                    >
                      {t("InstanceSyncPage.ledger.action.viewChanges")}
                    </Button>
                    <Button
                      size="xs"
                      colorScheme={primaryColor}
                      isLoading={pickingId === instance.id}
                      isDisabled={installing || pickingId !== null}
                      onClick={() => updateFromFile(instance.id, instance.name)}
                    >
                      {t("InstanceSyncPage.ledger.action.update")}
                    </Button>
                  </HStack>
                ),
              };
            })}
          />
        </Section>
      )}
    </VStack>
  );
};
