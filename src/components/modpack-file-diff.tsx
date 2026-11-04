import { Box, Button, Text } from "@chakra-ui/react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
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

interface ModpackFileDiffRowProps {
  instanceId: string;
  planHash: string;
  path: string;
  /** Set to let the parent hold the state, as a row in a virtualized list must. */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}

/** Diffs already read, so a row that scrolls back into view does not ask again. */
const diffCache = new Map<string, ModpackFileDiff | null>();

/**
 * A button that shows the line diff of one file of the update plan. The diff
 * is only asked for when it is opened, and again only for another plan.
 */
export const ModpackFileDiffRow: React.FC<ModpackFileDiffRowProps> = ({
  instanceId,
  planHash,
  path,
  open: controlledOpen,
  onOpenChange,
}) => {
  const { t } = useTranslation();
  const [ownOpen, setOwnOpen] = useState(false);
  const open = controlledOpen ?? ownOpen;
  const setOpen = (next: boolean) => {
    setOwnOpen(next);
    onOpenChange?.(next);
  };
  const cacheKey = `${instanceId}\0${planHash}\0${path}`;
  // undefined while loading, null when the request failed.
  const [diff, setDiff] = useState<ModpackFileDiff | null | undefined>(() =>
    diffCache.get(cacheKey)
  );

  useEffect(() => {
    if (!open) return;
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
  }, [open, instanceId, planHash, path, cacheKey]);

  const notice =
    diff === undefined ? null : diff ? fileDiffNotice(diff) : "unavailable";
  return (
    <Box px={3} pb={1.5}>
      <Button variant="link" size="xs" onClick={() => setOpen(!open)}>
        {t(`UpdateModpackModal.plan.fileDiff.${open ? "hide" : "show"}`)}
      </Button>
      {open &&
        (diff === undefined ? (
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
        ))}
    </Box>
  );
};
