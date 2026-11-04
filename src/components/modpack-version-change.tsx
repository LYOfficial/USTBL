import { HStack, Icon, Tag, Text } from "@chakra-ui/react";
import { useTranslation } from "react-i18next";
import { LuArrowRight } from "react-icons/lu";

/** How a named thing with a version differs between two versions of a pack. */
export type VersionChangeKind = "added" | "removed" | "changed";

export const versionChangeKind = (
  from?: string | null,
  to?: string | null
): VersionChangeKind =>
  from == null ? "added" : to == null ? "removed" : "changed";

/** Tag colors follow the file changes: added green, removed red, updated blue. */
export const VERSION_CHANGE_COLOR: Record<VersionChangeKind, string> = {
  added: "green",
  removed: "red",
  changed: "blue",
};

const Version: React.FC<{ value: string; struck?: boolean }> = ({
  value,
  struck,
}) => (
  <Text
    as="span"
    fontFamily="mono"
    fontSize="xs"
    textDecoration={struck ? "line-through" : undefined}
  >
    {value}
  </Text>
);

/**
 * One row of the component or loader changes: the fixed name, then the
 * versions as `old → new`. An added item shows only its new version, a removed
 * one only the old.
 */
export const VersionChangeRow: React.FC<{
  name: string;
  from?: string | null;
  to?: string | null;
}> = ({ name, from, to }) => {
  const { t } = useTranslation();
  const kind = versionChangeKind(from, to);
  return (
    <HStack spacing={2} fontSize="xs-sm" data-change={kind}>
      <Tag size="sm" variant="subtle" flexShrink={0}>
        {name}
      </Tag>
      {kind === "added" && (
        <>
          <Tag
            size="sm"
            colorScheme={VERSION_CHANGE_COLOR.added}
            flexShrink={0}
          >
            {t("UpdateModpackModal.plan.componentChanges.addedTag")}
          </Tag>
          <Version value={to || "-"} />
        </>
      )}
      {kind === "removed" && (
        <>
          <Tag
            size="sm"
            colorScheme={VERSION_CHANGE_COLOR.removed}
            flexShrink={0}
          >
            {t("UpdateModpackModal.plan.componentChanges.removedTag")}
          </Tag>
          <Version value={from || "-"} struck />
        </>
      )}
      {kind === "changed" && (
        <>
          <Version value={from || "-"} />
          <Icon
            as={LuArrowRight}
            color={`${VERSION_CHANGE_COLOR.changed}.500`}
            flexShrink={0}
          />
          <Tag
            size="sm"
            colorScheme={VERSION_CHANGE_COLOR.changed}
            flexShrink={0}
          >
            <Version value={to || "-"} />
          </Tag>
        </>
      )}
    </HStack>
  );
};
