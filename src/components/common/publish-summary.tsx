import { HStack, Tag, TagLabel, Text, Tooltip, VStack } from "@chakra-ui/react";
import { useTranslation } from "react-i18next";
import {
  PublishRule,
  PublishSummary as PublishSummaryData,
  isSummaryEmpty,
} from "@/utils/modpack-publish-tree";

const LISTED = 12;

interface Row {
  key: keyof Omit<PublishSummaryData, "isNew" | "pushed">;
  colorScheme: string;
}

const ROWS: Row[] = [
  { key: "added", colorScheme: "green" },
  { key: "modified", colorScheme: "orange" },
  { key: "onceUpdated", colorScheme: "cyan" },
  { key: "removed", colorScheme: "gray" },
  { key: "ignored", colorScheme: "gray" },
  { key: "deleted", colorScheme: "red" },
  { key: "toggled", colorScheme: "purple" },
  { key: "onceOn", colorScheme: "cyan" },
  { key: "onceOff", colorScheme: "cyan" },
  { key: "rulesAdded", colorScheme: "blue" },
  { key: "rulesRemoved", colorScheme: "blue" },
  { key: "rulesChanged", colorScheme: "blue" },
  { key: "localDeleted", colorScheme: "red" },
];

const nameOf = (
  item: string | PublishRule,
  t: (key: string) => string
): string =>
  typeof item === "string"
    ? item
    : `${item.path}${item.kind === "dir" ? "/" : ""} (${t(
        `PublishModpackModal.tree.${item.state}`
      )})`;

interface PublishSummaryProps {
  summary: PublishSummaryData;
}

/** What this version changes against the previous release, as it will be pushed. */
const PublishSummary: React.FC<PublishSummaryProps> = ({ summary }) => {
  const { t } = useTranslation();
  const rows = ROWS.filter((row) => summary[row.key].length > 0);

  return (
    <VStack align="stretch" spacing={1} w="100%" aria-live="polite">
      <Text fontSize="xs" fontWeight="bold">
        {t("PublishModpackModal.summary.title")}
      </Text>
      {summary.isNew && (
        <Text fontSize="xs" className="secondary-text">
          {t("PublishModpackModal.summary.isNew")}
        </Text>
      )}
      {isSummaryEmpty(summary) ? (
        <Text fontSize="xs" className="secondary-text">
          {t("PublishModpackModal.summary.same", { count: summary.pushed })}
        </Text>
      ) : (
        <HStack spacing={2} flexWrap="wrap">
          {rows.map(({ key, colorScheme }) => {
            const items: (string | PublishRule)[] = summary[key];
            const shown = items.slice(0, LISTED).map((item) => nameOf(item, t));
            return (
              <Tooltip
                key={key}
                label={
                  <>
                    <Text>{t(`PublishModpackModal.summary.hint.${key}`)}</Text>
                    {shown.map((name) => (
                      <Text key={name} mt={1} fontSize="xs">
                        {name}
                      </Text>
                    ))}
                    {items.length > shown.length && (
                      <Text mt={1} fontSize="xs" opacity={0.8}>
                        {t("PublishModpackModal.summary.more", {
                          count: items.length - shown.length,
                        })}
                      </Text>
                    )}
                  </>
                }
              >
                <Tag size="sm" variant="subtle" colorScheme={colorScheme}>
                  <TagLabel>
                    {t(`PublishModpackModal.summary.${key}`, {
                      count: items.length,
                    })}
                  </TagLabel>
                </Tag>
              </Tooltip>
            );
          })}
        </HStack>
      )}
    </VStack>
  );
};

export default PublishSummary;
