import { HStack, Text } from "@chakra-ui/react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useTranslation } from "react-i18next";
import { CommonIconButton } from "@/components/common/common-icon-button";
import { OptionItemGroup, OptionItemGroupProps } from "@/components/common/option-item";
import { TitleFullWithLogo } from "@/components/logo-title";
import { useLauncherConfig } from "@/contexts/config";

const AboutSettingsPage = () => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const basicInfo = config.basicInfo;

  const aboutSettingGroups: OptionItemGroupProps[] = [
    {
      title: t("AboutSettingsPage.about.title"),
      items: [
        <TitleFullWithLogo key={0} />,
        {
          title: t("AboutSettingsPage.about.settings.version.title"),
          children: (
            <Text fontSize="xs-sm" className="secondary-text">
              {`${basicInfo.launcherVersion}${basicInfo.isPortable ? " (Portable)" : ""}`}
            </Text>
          ),
        },
      ],
    },
    {
      title: t("AboutSettingsPage.ack.title"),
      items: [
        {
          title: "SJMCL：提供开源代码和开发思路",
          children: (
            <HStack>
              <CommonIconButton
                label="https://mc.sjtu.cn/sjmcl/"
                icon="external"
                withTooltip
                tooltipPlacement="bottom-end"
                size="xs"
                h={18}
                onClick={() => openUrl("https://mc.sjtu.cn/sjmcl/")}
              />
            </HStack>
          ),
        },
      ],
    },
  ];

  return (
    <>
      {aboutSettingGroups.map((group, index) => (
        <OptionItemGroup title={group.title} items={group.items} key={index} />
      ))}
    </>
  );
};

export default AboutSettingsPage;
