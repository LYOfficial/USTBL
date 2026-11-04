import { Box, Button, Text } from "@chakra-ui/react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { LuFileDiff } from "react-icons/lu";
import { BeatLoader } from "react-spinners";
import { DiffHunks } from "@/components/common/diff-view";
import { ModpackFileDiff } from "@/models/modpack-update";
import { ModpackUpdateService } from "@/services/modpack-update";

/** Which note replaces the diff, if the result has nothing to draw. */
export const fileDiffNotice = (
  diff: ModpackFileDiff
): "notText" | "unavailable" | "formatOnly" | null => {
  if (diff.unavailable) return diff.unavailable;
  if (!diff.hunks) return "unavailable";
  return diff.hunks.length === 0 ? "formatOnly" : null;
};

interface ModpackFileDiffButtonProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** The in-row button that opens the line diff below the file name. */
export const ModpackFileDiffButton: React.FC<ModpackFileDiffButtonProps> = ({
  open,
  onOpenChange,
}) => {
  const { t } = useTranslation();
  return (
    <Button
      size="xs"
      variant="ghost"
      leftIcon={<LuFileDiff />}
      flexShrink={0}
      aria-expanded={open}
      onClick={() => onOpenChange(!open)}
    >
      {t(`UpdateModpackModal.plan.fileDiff.${open ? "hide" : "show"}`)}
    </Button>
  );
};

interface ModpackFileDiffPanelProps {
  instanceId: string;
  planHash: string;
  path: string;
}

/** Diffs already read, so a row that scrolls back into view does not ask again. */
const diffCache = new Map<string, ModpackFileDiff | null>();

/**
 * The line diff of one file of the update plan. It mounts only while open, is
 * asked for once, and is read from the cache when its row is drawn again.
 */
export const ModpackFileDiffPanel: React.FC<ModpackFileDiffPanelProps> = ({
  instanceId,
  planHash,
  path,
}) => {
  const { t } = useTranslation();
  const cacheKey = `${instanceId}\0${planHash}\0${path}`;
  // undefined while loading, null when the request failed.
  const [diff, setDiff] = useState<ModpackFileDiff | null | undefined>(() =>
    diffCache.get(cacheKey)
  );

  useEffect(() => {
    if (diffCache.has(cacheKey)) {
      setDiff(diffCache.get(cacheKey));
      return;
    }
    let isActive = true;
    setDiff(undefined);
    ModpackUpdateService.modpackUpdateFileDiff(instanceId, planHash, path).then(
      (response) => {
        // A failed request is not kept; opening the row again retries.
        if (response.status === "success") {
          diffCache.set(cacheKey, response.data);
        }
        if (isActive) {
          setDiff(response.status === "success" ? response.data : null);
        }
      }
    );
    return () => {
      isActive = false;
    };
  }, [instanceId, planHash, path, cacheKey]);

  const notice =
    diff === undefined ? null : diff ? fileDiffNotice(diff) : "unavailable";
  return (
    <Box px={3} pb={1.5}>
      {diff === undefined ? (
        <Box mt={1}>
          <BeatLoader size={6} color="gray" />
        </Box>
      ) : notice ? (
        <Text fontSize="xs" className="secondary-text" mt={1}>
          {t(`UpdateModpackModal.plan.fileDiff.${notice}`)}
        </Text>
      ) : (
        <Box
          mt={1}
          maxH="64"
          overflow="auto"
          borderWidth="1px"
          borderRadius="md"
        >
          <DiffHunks hunks={diff?.hunks ?? []} />
        </Box>
      )}
    </Box>
  );
};
