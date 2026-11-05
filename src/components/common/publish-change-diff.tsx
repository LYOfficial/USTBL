import { Box, Text } from "@chakra-ui/react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { BeatLoader } from "react-spinners";
import { DiffHunks } from "@/components/common/diff-view";
import { PublishFileDiff } from "@/models/modpack-update";
import { CampusModpackService } from "@/services/campus-modpack";
import { diffReasonKey } from "@/utils/diff-reason";
import { PublishChange } from "@/utils/modpack-publish-tree";
import { formatByteSize } from "@/utils/string";

interface PublishChangeDiffProps {
  instanceId: string;
  change: PublishChange;
}

/**
 * How a file's content differs from the previous release.
 */
const PublishChangeDiff: React.FC<PublishChangeDiffProps> = ({
  instanceId,
  change,
}) => {
  const { t } = useTranslation();
  // undefined while loading.
  const [diff, setDiff] = useState<PublishFileDiff | null | undefined>();

  useEffect(() => {
    if (!change.previousSha1) {
      setDiff({ hunks: null, unavailable: "baseMissing" });
      return;
    }
    let isActive = true;
    setDiff(undefined);
    CampusModpackService.diffCampusModpackPublishFile(
      instanceId,
      change.path,
      change.previousSha1,
      change.previousVersionId ?? "",
      change.previousProjectId ?? ""
    ).then((response) => {
      if (isActive) {
        if (response.status === "success") {
          setDiff(response.data);
        } else {
          setDiff(null);
        }
      }
    });
    return () => {
      isActive = false;
    };
  }, [
    instanceId,
    change.path,
    change.previousSha1,
    change.previousProjectId,
    change.previousVersionId,
  ]);

  const sizes = {
    previous: formatByteSize(change.previousSize ?? 0),
    local: formatByteSize(change.localSize ?? 0),
  };

  if (diff === undefined) {
    return <BeatLoader size={6} color="gray" />;
  }
  if (diff === null || diff.hunks === null || diff.hunks.length === 0) {
    const reasonKey = diffReasonKey(diff?.unavailable);
    return (
      <Text fontSize="xs" className="secondary-text">
        {t(
          reasonKey ??
            (diff === null
              ? "PublishModpackModal.tree.diff.unavailable"
              : diff.hunks === null
                ? "PublishModpackModal.tree.diff.differs"
                : "PublishModpackModal.tree.diff.formatOnly"),
          sizes
        )}
      </Text>
    );
  }
  return (
    <Box maxH="64" overflow="auto" borderWidth="1px" borderRadius="md">
      <DiffHunks hunks={diff.hunks} />
    </Box>
  );
};

export default PublishChangeDiff;
