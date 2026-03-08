import { Box, BoxProps, HStack, useToken } from "@chakra-ui/react";
import React from "react";

interface MultiLevelProgressBarProps extends BoxProps {
  value: number | number[];
  colorScheme: string;
}

const MultiLevelProgressBar: React.FC<MultiLevelProgressBarProps> = ({
  value,
  colorScheme,
  ...boxProps
}) => {
  const values = Array.isArray(value) ? value : [value];

  const baseColorDepths = [500, 400, 300, 200];
  const colorKeys = values.map(
    (_, index) =>
      `${colorScheme}.${baseColorDepths[index * Math.floor(baseColorDepths.length / values.length)] || 200}`
  );
  const colors = useToken("colors", colorKeys);

  return (
    <HStack
      w="100%"
      h={3}
      spacing={0}
      bg="gray.200"
      overflow="hidden"
      {...boxProps}
    >
      {values.map((percent, index) => (
        <Box
          key={index}
          h="100%"
          w={`${percent}%`}
          bg={colors[index] || colors[colors.length - 1]}
          flexShrink={0}
        />
      ))}
    </HStack>
  );
};

export default MultiLevelProgressBar;
