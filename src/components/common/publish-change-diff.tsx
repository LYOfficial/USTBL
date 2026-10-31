import { Box, Text } from "@chakra-ui/react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { BeatLoader } from "react-spinners";
import { DiffHunks } from "@/components/common/diff-view";
import { ModpackDiffHunk } from "@/models/modpack-update";
import { CampusModpackService } from "@/services/campus-modpack";
import { PublishChange } from "@/utils/modpack-publish-tree";
import { formatByteSize } from "@/utils/string";

interface PublishChangeDiffProps {
  instanceId: string;
  change: PublishChange;
}

/**
 * How a file's content differs from the previous release. The launcher keeps
 * the previous text only for files it installed itself; without it, or for a
 * file that is not text, only the two sizes can be told.
 */
const PublishChangeDiff: React.FC<PublishChangeDiffProps> = ({
  instanceId,
  change,
}) => {
  const { t } = useTranslation();
  // undefined while loading, null when no diff can be drawn.
  const [hunks, setHunks] = useState<ModpackDiffHunk[] | null | undefined>();

  useEffect(() => {
    if (!change.previousSha1) {
      setHunks(null);
      return;
    }
    let isActive = true;
    setHunks(undefined);
    CampusModpackService.diffCampusModpackPublishFile(
      instanceId,
      change.path,
      change.previousSha1
    ).then((response) => {
      if (isActive) {
        setHunks(response.status === "success" ? response.data : null);
      }
    });
    return () => {
      isActive = false;
    };
  }, [instanceId, change.path, change.previousSha1]);

  const sizes = {
    previous: formatByteSize(change.previousSize ?? 0),
    local: formatByteSize(change.localSize ?? 0),
  };

  if (hunks === undefined) {
    return <BeatLoader size={6} color="gray" />;
  }
  if (hunks === null || hunks.length === 0) {
    return (
      <Text fontSize="xs" className="secondary-text">
        {t(
          hunks === null
            ? "PublishModpackModal.tree.diff.differs"
            : "PublishModpackModal.tree.diff.formatOnly",
          sizes
        )}
      </Text>
    );
  }
  return (
    <Box maxH="64" overflow="auto" borderWidth="1px" borderRadius="md">
      <DiffHunks hunks={hunks} />
    </Box>
  );
};

export default PublishChangeDiff;
