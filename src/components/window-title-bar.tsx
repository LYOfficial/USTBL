import { HStack, Icon, IconButton, Image, Text } from "@chakra-ui/react";
import { useEffect, useState } from "react";
import { LuCopy, LuMinus, LuSquare, LuX } from "react-icons/lu";

const WindowTitleBar = () => {
  const [isMaximized, setIsMaximized] = useState(false);

  useEffect(() => {
    let unlisten: (() => void) | undefined;

    const setup = async () => {
      if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
        return;
      }

      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const appWindow = getCurrentWindow();

      setIsMaximized(await appWindow.isMaximized());
      unlisten = await appWindow.onResized(async () => {
        setIsMaximized(await appWindow.isMaximized());
      });
    };

    setup();

    return () => {
      unlisten?.();
    };
  }, []);

  const onMinimize = async () => {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().minimize();
  };

  const onToggleMaximize = async () => {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const appWindow = getCurrentWindow();
    await appWindow.toggleMaximize();
    setIsMaximized(await appWindow.isMaximized());
  };

  const onClose = async () => {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().close();
  };

  return (
    <HStack
      justify="space-between"
      h="32px"
      px={2}
      data-tauri-drag-region
      borderBottomWidth="1px"
      borderColor="blackAlpha.200"
      bg="blackAlpha.200"
      _dark={{ borderColor: "whiteAlpha.200", bg: "whiteAlpha.100" }}
      userSelect="none"
    >
      <HStack spacing={2} data-tauri-drag-region>
        <Image
          src="/images/icons/Logo_128x128.png"
          alt="USTBL"
          boxSize="14px"
          data-tauri-drag-region
        />
        <Text fontSize="xs" fontWeight="600" data-tauri-drag-region>
          USTBL
        </Text>
      </HStack>

      <HStack spacing={0.5}>
        <IconButton
          aria-label="minimize"
          size="xs"
          variant="ghost"
          icon={<Icon as={LuMinus} />}
          onClick={onMinimize}
        />
        <IconButton
          aria-label="maximize"
          size="xs"
          variant="ghost"
          icon={<Icon as={isMaximized ? LuCopy : LuSquare} />}
          onClick={onToggleMaximize}
        />
        <IconButton
          aria-label="close"
          size="xs"
          variant="ghost"
          colorScheme="red"
          icon={<Icon as={LuX} />}
          onClick={onClose}
        />
      </HStack>
    </HStack>
  );
};

export default WindowTitleBar;
