import { Button, ButtonProps, Tooltip } from "@chakra-ui/react";
import { ComponentProps } from "react";
import { useTranslation } from "react-i18next";
import { CommonIconButton } from "@/components/common/common-icon-button";
import { InstanceSummary } from "@/models/instance/misc";
import { isInstallingInstance } from "@/utils/installing-instance";

interface InstanceLaunchProps {
  instance?: InstanceSummary;
}

const installingReasonKey =
  "Services.launch.selectSuitableJRE.error.description.LAUNCH_INSTANCE_INSTALLING";

export const InstanceLaunchButton = ({
  instance,
  ...props
}: ButtonProps & InstanceLaunchProps) => {
  const { t } = useTranslation();
  const installing = isInstallingInstance(instance);
  if (!installing) return <Button {...props} />;
  return (
    <Tooltip label={t(installingReasonKey)} shouldWrapChildren>
      <Button {...props} isDisabled />
    </Tooltip>
  );
};

export const InstanceLaunchIconButton = ({
  instance,
  ...props
}: ComponentProps<typeof CommonIconButton> & InstanceLaunchProps) => {
  const { t } = useTranslation();
  const installing = isInstallingInstance(instance);
  if (!installing) return <CommonIconButton {...props} />;
  return (
    <Tooltip label={t(installingReasonKey)} shouldWrapChildren>
      <CommonIconButton {...props} withTooltip={false} isDisabled />
    </Tooltip>
  );
};
