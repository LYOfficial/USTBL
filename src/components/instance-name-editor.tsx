import { Box, Input, Text, Tooltip, VStack } from "@chakra-ui/react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { INSTANCE_TOOLTIP_DELAY_MS } from "@/components/instance-sidebar-parts";
import { useLauncherConfig } from "@/contexts/config";
import { useRenameInstance } from "@/hooks/rename-instance";
import { InstanceSummary } from "@/models/instance/misc";
import { getGameDirName } from "@/utils/instance";

interface InstanceNameEditorProps {
  instance: InstanceSummary;
  onRenamed?: (name: string) => void;
}

// The instance name as a title: double-click to edit in place, with the
// directory it lives in shown on hover.
export const InstanceNameEditor: React.FC<InstanceNameEditorProps> = ({
  instance,
  onRenamed,
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const rename = useRenameInstance();
  const [isEditing, setIsEditing] = useState(false);
  const [draft, setDraft] = useState(instance.name);
  const cancelledRef = useRef(false);

  useEffect(() => {
    if (!isEditing) setDraft(instance.name);
  }, [instance.name, isEditing]);

  const finish = async (save: boolean) => {
    setIsEditing(false);
    const next = draft.trim();
    // An empty name keeps the current one.
    if (!save || !next || next === instance.name) {
      setDraft(instance.name);
      return;
    }
    const shown = await rename(instance.id, next);
    if (shown === null) setDraft(instance.name);
    else onRenamed?.(shown);
  };

  if (isEditing) {
    return (
      <Input
        autoFocus
        size="xs"
        h={21}
        fontWeight="bold"
        fontSize="sm"
        value={draft}
        w={`${Math.min(Math.max(draft.length + 2, 12), 48)}ch`}
        focusBorderColor={`${primaryColor}.500`}
        onFocus={(e) => e.target.select()}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            e.currentTarget.blur();
          } else if (e.key === "Escape") {
            cancelledRef.current = true;
            e.currentTarget.blur();
          }
        }}
        onBlur={() => {
          const cancelled = cancelledRef.current;
          cancelledRef.current = false;
          void finish(!cancelled);
        }}
      />
    );
  }

  return (
    <Tooltip
      label={
        <VStack spacing={0} align="start">
          <Text fontSize="xs">{instance.name}</Text>
          <Text fontSize="xs" opacity={0.8}>
            {t("InstanceNameEditor.tooltip.location", {
              gameDir: getGameDirName(instance.gameDir),
              folder: instance.folder,
            })}
          </Text>
        </VStack>
      }
      openDelay={INSTANCE_TOOLTIP_DELAY_MS}
      closeOnPointerDown
    >
      <Box as="span" onDoubleClick={() => setIsEditing(true)}>
        {instance.name}
      </Box>
    </Tooltip>
  );
};

export default InstanceNameEditor;
