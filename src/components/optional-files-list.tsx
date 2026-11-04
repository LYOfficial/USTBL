import { Button, Checkbox, HStack, Text, VStack } from "@chakra-ui/react";
import { useTranslation } from "react-i18next";
import { ModpackOptionalFile } from "@/models/modpack-update";
import {
  invertOptional,
  selectAllOptional,
  selectNoneOptional,
  toggleOptional,
} from "@/utils/modpack-optional";

interface OptionalFilesListProps {
  files: ModpackOptionalFile[];
  selected: string[];
  onChange: (selected: string[]) => void;
  isDisabled?: boolean;
}

export const OptionalFilesList: React.FC<OptionalFilesListProps> = ({
  files,
  selected,
  onChange,
  isDisabled,
}) => {
  const { t } = useTranslation();

  return (
    <VStack align="stretch" spacing={2} w="100%">
      <HStack spacing={2}>
        <Button
          size="xs"
          variant="outline"
          isDisabled={isDisabled}
          onClick={() => onChange(selectAllOptional(files))}
        >
          {t("OptionalFiles.button.selectAll")}
        </Button>
        <Button
          size="xs"
          variant="outline"
          isDisabled={isDisabled}
          onClick={() => onChange(selectNoneOptional())}
        >
          {t("OptionalFiles.button.selectNone")}
        </Button>
        <Button
          size="xs"
          variant="outline"
          isDisabled={isDisabled}
          onClick={() => onChange(invertOptional(files, selected))}
        >
          {t("OptionalFiles.button.invert")}
        </Button>
      </HStack>
      <VStack align="stretch" spacing={1}>
        {files.map((file) => (
          <Checkbox
            key={file.key}
            isDisabled={isDisabled}
            isChecked={selected.includes(file.key)}
            onChange={() => onChange(toggleOptional(selected, file.key))}
          >
            <Text fontSize="xs-sm" wordBreak="break-all">
              {file.name}
            </Text>
          </Checkbox>
        ))}
      </VStack>
    </VStack>
  );
};
