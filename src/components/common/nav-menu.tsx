import { Stack, StackProps, Tooltip } from "@chakra-ui/react";
import React from "react";
import SelectableButton from "@/components/common/selectable-button";

export interface MenuItem {
  label: React.ReactNode;
  value: any;
  tooltip?: string;
}

export interface NavMenuProps extends StackProps {
  items: MenuItem[];
  selectedKeys?: any[];
  onClick?: (value: any) => void;
  size?: string;
}

const NavMenu: React.FC<NavMenuProps> = ({
  items,
  selectedKeys = [],
  onClick,
  size = "sm",
  ...props
}) => {
  return (
    <Stack align="stretch" spacing={0.5} {...props}>
      {items.map((item) => (
        <Tooltip label={item.tooltip} placement="right" key={item.value}>
          <Stack align="stretch" direction="column">
            <SelectableButton
              key={item.value}
              size={size}
              isSelected={selectedKeys.includes(item.value)}
              onClick={() => onClick && onClick(item.value)}
            >
              {item.label}
            </SelectableButton>
          </Stack>
        </Tooltip>
      ))}
    </Stack>
  );
};

export default NavMenu;
