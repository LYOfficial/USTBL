import {
  IconButton,
  Input,
  InputGroup,
  InputGroupProps,
  InputLeftElement,
  InputRightElement,
} from "@chakra-ui/react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { LuSearch, LuX } from "react-icons/lu";
import { useLauncherConfig } from "@/contexts/config";

export const FILTER_DEBOUNCE_MS = 150;

interface FileFilterInputProps extends Omit<InputGroupProps, "onChange"> {
  /** The committed query, which trails what is typed by the debounce. */
  value: string;
  onChange: (value: string) => void;
}

/**
 * Small search box for filtering a list of files. Typing is held back for a
 * moment before `onChange` fires so a long list is not refiltered per key.
 */
const FileFilterInput: React.FC<FileFilterInputProps> = ({
  value,
  onChange,
  ...groupProps
}) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const [text, setText] = useState(value);
  const emitted = useRef(value);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  useEffect(() => {
    if (value !== emitted.current) {
      emitted.current = value;
      setText(value);
    }
  }, [value]);

  useEffect(() => {
    if (text === emitted.current) return;
    const timer = setTimeout(() => {
      emitted.current = text;
      onChangeRef.current(text);
    }, FILTER_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [text]);

  const clear = () => {
    emitted.current = "";
    setText("");
    onChange("");
  };

  return (
    <InputGroup size="xs" {...groupProps}>
      <InputLeftElement h="100%" pointerEvents="none">
        <LuSearch />
      </InputLeftElement>
      <Input
        focusBorderColor={`${primaryColor}.500`}
        borderRadius="md"
        placeholder={t("FileFilter.placeholder")}
        value={text}
        onChange={(event) => setText(event.target.value)}
      />
      {text && (
        <InputRightElement h="100%">
          <IconButton
            size="xs"
            variant="ghost"
            aria-label={t("FileFilter.clear")}
            icon={<LuX />}
            onClick={clear}
          />
        </InputRightElement>
      )}
    </InputGroup>
  );
};

export default FileFilterInput;
