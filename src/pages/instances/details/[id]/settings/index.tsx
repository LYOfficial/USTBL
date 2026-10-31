import {
  Box,
  Button,
  Collapse,
  HStack,
  Image,
  Link,
  Switch,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useRouter } from "next/router";
import { useCallback } from "react";
import { Trans, useTranslation } from "react-i18next";
import Editable from "@/components/common/editable";
import {
  OptionItemGroup,
  OptionItemGroupProps,
} from "@/components/common/option-item";
import GameSettingsGroups from "@/components/game-settings-groups";
import { InstanceIconSelectorPopover } from "@/components/instance-icon-selector";
import { useLauncherConfig } from "@/contexts/config";
import { useInstanceSharedData } from "@/contexts/instance";
import { useSharedModals } from "@/contexts/shared-modal";
import { useToast } from "@/contexts/toast";
import { InstanceService } from "@/services/instance";
import {
  getInstanceIconSrc,
  isInstanceNameInvalid,
  ownsInstanceFiles,
} from "@/utils/instance";

const getSharedGameDirectory = (versionPath: string) =>
  versionPath.replace(/[\\/][^\\/]+[\\/][^\\/]+$/, "");

const InstanceSettingsPage = () => {
  const router = useRouter();
  const toast = useToast();
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const { openGenericConfirmDialog } = useSharedModals();

  const { id } = router.query;
  const instanceId = Array.isArray(id) ? id[0] : id;

  const {
    summary,
    updateSummaryInContext,
    gameConfig: instanceGameConfig,
    handleUpdateInstanceConfig,
    handleRestoreInstanceGameConfig,
  } = useInstanceSharedData();
  const useSpecGameConfig = summary?.useSpecGameConfig || false;

  const getDirectoryChange = (nextIsolated: boolean) => ({
    from: summary?.isVersionIsolated
      ? summary.versionPath
      : getSharedGameDirectory(summary?.versionPath || ""),
    to: nextIsolated
      ? summary?.versionPath || ""
      : getSharedGameDirectory(summary?.versionPath || ""),
  });

  const confirmIsolationChange = (
    nextIsolated: boolean,
    onConfirm: () => void
  ) => {
    if (summary?.isolationSource === "locked") {
      onConfirm();
      return;
    }
    const { from, to } = getDirectoryChange(nextIsolated);
    const lockWarning =
      nextIsolated && summary && ownsInstanceFiles(summary)
        ? ` ${t("VersionIsolationConfirmDialog.lockWarning")}`
        : "";
    openGenericConfirmDialog({
      title: t("VersionIsolationConfirmDialog.title"),
      body: `${t("VersionIsolationConfirmDialog.body", { from, to })}${lockWarning}`,
      onOKCallback: onConfirm,
    });
  };

  const handleRenameInstance = useCallback(
    (name: string) => {
      if (!instanceId) return;
      InstanceService.renameInstance(instanceId, name).then((response) => {
        if (response.status === "success") {
          updateSummaryInContext("versionPath", response.data);
          updateSummaryInContext("name", name);
        } else
          toast({
            title: response.message,
            description: response.details,
            status: "error",
          });
      });
    },
    [instanceId, toast, updateSummaryInContext]
  );

  const instanceSpecSettingsGroups: OptionItemGroupProps[] = [
    {
      items: [
        {
          title: t("InstanceSettingsPage.name"),
          children: (
            <Editable
              isTextArea={false}
              value={summary?.name || ""}
              onEditSubmit={handleRenameInstance}
              textProps={{ className: "secondary-text", fontSize: "xs-sm" }}
              inputProps={{ fontSize: "xs-sm" }}
              formErrMsgProps={{ fontSize: "xs-sm" }}
              checkError={isInstanceNameInvalid}
              localeKey="InstanceSettingsPage.errorMessage"
              flex={1}
            />
          ),
        },
        {
          title: t("InstanceSettingsPage.description"),
          children: (
            <Editable
              isTextArea={true}
              value={summary?.description || ""}
              onEditSubmit={(value) => {
                handleUpdateInstanceConfig("description", value);
              }}
              textProps={{ className: "secondary-text", fontSize: "xs-sm" }}
              inputProps={{ fontSize: "xs-sm" }}
              flex={1}
            />
          ),
        },
        {
          title: t("InstanceSettingsPage.icon"),
          children: (
            <HStack>
              <Image
                src={getInstanceIconSrc(summary?.iconSrc, summary?.versionPath)}
                alt={summary?.iconSrc}
                boxSize="28px"
                fallbackSrc="/images/icons/JEIcon_Release.png"
              />
              <InstanceIconSelectorPopover
                value={summary?.iconSrc}
                onIconSelect={(value) => {
                  handleUpdateInstanceConfig("iconSrc", value);
                }}
                versionPath={summary?.versionPath}
                instanceId={summary?.id}
              />
            </HStack>
          ),
        },
        {
          title: t("InstanceDetailsLayout.secMenu.star"),
          children: (
            <Switch
              colorScheme={primaryColor}
              isChecked={Boolean(summary?.starred)}
              onChange={(event) => {
                handleUpdateInstanceConfig("starred", event.target.checked);
              }}
            />
          ),
        },
      ],
    },
    {
      items: [
        {
          title: t("InstanceSettingsPage.applySettings"),
          children: (
            <Switch
              colorScheme={primaryColor}
              isChecked={useSpecGameConfig}
              onChange={(event) => {
                const nextUseSpec = event.target.checked;
                const isLocked = summary?.isolationSource === "locked";
                const nextIsolated = isLocked
                  ? undefined
                  : nextUseSpec
                    ? (summary?.specVersionIsolation ??
                      config.globalGameConfig.versionIsolation)
                    : config.globalGameConfig.versionIsolation;
                const update = () =>
                  handleUpdateInstanceConfig("useSpecGameConfig", nextUseSpec);
                if (
                  !isLocked &&
                  summary &&
                  nextIsolated !== undefined &&
                  nextIsolated !== summary.isVersionIsolated
                ) {
                  confirmIsolationChange(nextIsolated, update);
                } else {
                  update();
                }
              }}
            />
          ),
        },
        ...(useSpecGameConfig && instanceGameConfig
          ? [
              {
                title: t("InstanceSettingsPage.restoreSettings"),
                description: t("InstanceSettingsPage.restoreSettingsDesc"),
                children: (
                  <Button
                    colorScheme="red"
                    variant="subtle"
                    size="xs"
                    onClick={() => {
                      openGenericConfirmDialog({
                        title: t("RestoreInstanceConfigConfirmDialog.title"),
                        body: t("RestoreInstanceConfigConfirmDialog.body"),
                        isAlert: true,
                        onOKCallback: handleRestoreInstanceGameConfig,
                        showSuppressBtn: true,
                        suppressKey: "restoreInstanceSpecConfig",
                      });
                    }}
                  >
                    {t("InstanceSettingsPage.restore")}
                  </Button>
                ),
              },
            ]
          : []),
        {
          title: t("InstanceSettingsPage.versionIsolation.title"),
          description:
            summary?.isolationSource === "locked"
              ? t("InstanceSettingsPage.versionIsolation.locked")
              : summary?.isolationSource === "instance"
                ? t("InstanceSettingsPage.versionIsolation.instance", {
                    state: summary.isVersionIsolated
                      ? t("InstanceSettingsPage.versionIsolation.on")
                      : t("InstanceSettingsPage.versionIsolation.off"),
                  })
                : t("InstanceSettingsPage.versionIsolation.global", {
                    state: summary?.isVersionIsolated
                      ? t("InstanceSettingsPage.versionIsolation.on")
                      : t("InstanceSettingsPage.versionIsolation.off"),
                  }),
          children:
            summary?.isolationSource === "locked" ? (
              <Switch colorScheme={primaryColor} isChecked isDisabled />
            ) : summary?.isolationSource === "instance" ? (
              <Switch
                colorScheme={primaryColor}
                isChecked={summary.isVersionIsolated}
                onChange={(event) => {
                  const nextIsolated = event.target.checked;
                  confirmIsolationChange(nextIsolated, () =>
                    handleUpdateInstanceConfig(
                      "specGameConfig.versionIsolation",
                      nextIsolated
                    )
                  );
                }}
              />
            ) : (
              <Link
                color={`${primaryColor}.500`}
                onClick={() => router.push("/settings/global-game")}
              >
                {t("InstanceSettingsPage.versionIsolation.goGlobal")}
              </Link>
            ),
        },
      ],
    },
  ];

  return (
    <Box height="100%" overflowY="auto">
      <VStack overflow="auto" align="stretch" spacing={4} flex="1">
        {instanceSpecSettingsGroups.map((group, index) => (
          <OptionItemGroup
            title={group.title}
            items={group.items}
            key={index}
          />
        ))}
        {!useSpecGameConfig && (
          <Text className="secondary-text" fontSize="xs-sm" textAlign="center">
            <Trans
              i18nKey="InstanceSettingsPage.tipsToGlobal.content"
              components={{
                terms: (
                  <Link
                    color={`${primaryColor}.500`}
                    onClick={() => {
                      router.push("/settings/global-game");
                    }}
                  />
                ),
              }}
            />
          </Text>
        )}
      </VStack>
      <Box h={4} />
      <Collapse in={useSpecGameConfig} animateOpacity>
        {useSpecGameConfig && instanceGameConfig && (
          <GameSettingsGroups
            gameConfig={instanceGameConfig}
            updateGameConfig={(key: string, value: any) => {
              handleUpdateInstanceConfig(`specGameConfig.${key}`, value);
            }}
          />
        )}
      </Collapse>
    </Box>
  );
};

export default InstanceSettingsPage;
