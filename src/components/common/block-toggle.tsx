import { Box, HStack, Text } from "@chakra-ui/react";
import { KeyboardEvent } from "react";
import { useLauncherConfig } from "@/contexts/config";

interface BlockToggleProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  children: React.ReactNode;
  "aria-label"?: string;
}

/** A compact, keyboard accessible switch whose label stays inside its border. */
const BlockToggle: React.FC<BlockToggleProps> = ({
  checked,
  onChange,
  children,
  "aria-label": ariaLabel,
}) => {
  const { config } = useLauncherConfig();
  const primaryColor = config.appearance.theme.primaryColor;
  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === " " || event.key === "Enter") {
      event.preventDefault();
      onChange(!checked);
    }
  };

  return (
    <HStack
      as="div"
      role="switch"
      aria-checked={checked}
      aria-label={ariaLabel}
      tabIndex={0}
      onClick={() => onChange(!checked)}
      onKeyDown={handleKeyDown}
      spacing={2}
      px={2}
      py={1}
      borderWidth="1px"
      borderColor={checked ? `${primaryColor}.400` : "gray.300"}
      borderRadius="sm"
      color={checked ? `${primaryColor}.600` : undefined}
      whiteSpace="nowrap"
      flexShrink={0}
      cursor="pointer"
      userSelect="none"
    >
      <Text fontSize="xs">{children}</Text>
      <Box
        w="12px"
        h="12px"
        flexShrink={0}
        borderWidth="1px"
        borderColor={checked ? `${primaryColor}.500` : "gray.400"}
        bg={checked ? `${primaryColor}.500` : "transparent"}
      />
    </HStack>
  );
};

export default BlockToggle;
