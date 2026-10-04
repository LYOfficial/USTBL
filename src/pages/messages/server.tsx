import {
  Box,
  Button,
  Flex,
  HStack,
  Input,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import { VustbServerMessage, VustbServerMessageGroup } from "@/models/vustb";
import { AccountService } from "@/services/account";

export default function MessagesServerPage() {
  const toast = useToast();
  const { selectedPlayer } = useGlobalData();
  const [groups, setGroups] = useState<VustbServerMessageGroup[]>([]);
  const [selectedId, setSelectedId] = useState<number>();
  const [content, setContent] = useState("");
  const [quiet, setQuiet] = useState<Record<number, boolean>>({});
  const [contextMenu, setContextMenu] = useState<{
    serverId: number;
    x: number;
    y: number;
  }>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const requestVersion = useRef(0);
  const nextRetryAt = useRef(0);
  const messageScrollRef = useRef<HTMLDivElement>(null);

  const load = async () => {
    if (Date.now() < nextRetryAt.current) return;
    const version = ++requestVersion.current;
    setLoading(true);
    const response = await AccountService.retrieveVustbServerMessages();
    if (version !== requestVersion.current) return;
    if (response.status === "success") {
      setGroups(response.data);
      setSelectedId((current) => current ?? response.data[0]?.id);
      setError(undefined);
      nextRetryAt.current = 0;
    } else {
      setError(response.details || response.message || "服务器消息加载失败");
      nextRetryAt.current = Date.now() + 60000;
    }
    setLoading(false);
  };

  useEffect(() => {
    void load();
    const timer = window.setInterval(() => void load(), 15000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    setQuiet(
      Object.fromEntries(
        groups.map((group) => [
          group.id,
          window.localStorage.getItem(
            `ustbl.message.quiet.server.${group.id}`
          ) === "true",
        ])
      )
    );
  }, [groups]);

  const selected = useMemo(
    () => groups.find((group) => group.id === selectedId),
    [groups, selectedId]
  );
  const latestMessageId = selected?.messages.length
    ? selected.messages[selected.messages.length - 1].id
    : undefined;
  useEffect(() => {
    const element = messageScrollRef.current;
    if (!element) return;
    const frame = window.requestAnimationFrame(() => {
      element.scrollTo({ top: element.scrollHeight, behavior: "smooth" });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [selectedId, latestMessageId]);
  const setServerQuiet = (serverId: number, value: boolean) => {
    setQuiet((current) => ({ ...current, [serverId]: value }));
    window.localStorage.setItem(
      `ustbl.message.quiet.server.${serverId}`,
      String(value)
    );
  };
  const send = async () => {
    if (!selected || !selectedPlayer?.name || !content.trim()) return;
    const response = await AccountService.sendVustbServerMessage(
      selected.id,
      selectedPlayer.name,
      content.trim()
    );
    if (response.status === "success") {
      setContent("");
      void load();
    } else
      toast({
        title: response.message,
        description: response.details,
        status: "error",
      });
  };

  return (
    <Flex
      h="100%"
      minH={0}
      position="relative"
      border="1px solid"
      borderColor="blackAlpha.200"
      rounded="md"
      overflow="hidden"
      onClick={() => setContextMenu(undefined)}
    >
      <VStack
        align="stretch"
        spacing={1}
        w={{ base: "42%", md: "30%" }}
        minW={0}
        borderRight="1px solid"
        borderColor="blackAlpha.200"
        overflowY="auto"
        p={2}
      >
        {loading && groups.length === 0 ? (
          <Text p={4} className="secondary-text">
            加载中…
          </Text>
        ) : error && groups.length === 0 ? (
          <Text p={4} color="red.500">
            {error}
          </Text>
        ) : groups.length === 0 ? (
          <Text p={4} className="secondary-text">
            暂无启用消息的服务器
          </Text>
        ) : (
          groups.map((group) => (
            <Button
              key={group.id}
              minH="68px"
              py={3}
              variant="ghost"
              bg={group.id === selectedId ? "blackAlpha.100" : undefined}
              border="1px solid"
              borderColor={group.id === selectedId ? "blue.400" : "transparent"}
              _hover={{
                bg:
                  group.id === selectedId ? "blackAlpha.100" : "blackAlpha.50",
              }}
              borderRadius="md"
              justifyContent="flex-start"
              textAlign="left"
              onClick={() => setSelectedId(group.id)}
              onContextMenu={(event) => {
                event.preventDefault();
                setSelectedId(group.id);
                setContextMenu({
                  serverId: group.id,
                  x: event.clientX,
                  y: event.clientY,
                });
              }}
            >
              <Box minW={0}>
                <Text noOfLines={1}>{group.name}</Text>
                <Text
                  fontSize="xs"
                  color={quiet[group.id] ? "orange.400" : "gray.500"}
                  noOfLines={1}
                >
                  {quiet[group.id] ? "免打扰" : group.address}
                </Text>
              </Box>
            </Button>
          ))
        )}
      </VStack>
      <Flex direction="column" flex={1} minW={0}>
        <Box ref={messageScrollRef} flex={1} overflowY="auto" p={3}>
          {selected?.messages.map((message: VustbServerMessage) => (
            <Box
              key={message.id}
              mb={2}
              p={2}
              rounded="md"
              bg={message.sourceType === "ustbl" ? "blackAlpha.100" : "blue.50"}
            >
              <HStack justify="space-between" spacing={2}>
                <Text fontWeight="600" fontSize="sm" noOfLines={1}>
                  {message.sender}
                </Text>
                <Text fontSize="xs" className="secondary-text" flexShrink={0}>
                  {new Date(message.createdAt).toLocaleTimeString()}
                </Text>
              </HStack>
              <Text mt={1} fontSize="sm" whiteSpace="pre-wrap">
                {message.content}
              </Text>
            </Box>
          ))}
          {!loading && !error && selected && selected.messages.length === 0 && (
            <Text className="secondary-text">暂无消息</Text>
          )}
        </Box>
        <HStack p={3} borderTop="1px solid" borderColor="blackAlpha.200">
          <Input
            value={content}
            onChange={(event) => setContent(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void send();
            }}
            placeholder={
              selectedPlayer?.name
                ? "输入纯文本消息"
                : "请选择像素北科游戏角色后发送"
            }
            isDisabled={!selectedPlayer?.name || !selected}
          />
          <Button
            colorScheme="blue"
            onClick={() => void send()}
            isDisabled={!selectedPlayer?.name || !selected}
          >
            发送
          </Button>
        </HStack>
      </Flex>
      {contextMenu && (
        <Box
          position="fixed"
          left={`${contextMenu.x}px`}
          top={`${contextMenu.y}px`}
          zIndex={20}
          bg="white"
          shadow="lg"
          border="1px solid"
          borderColor="blackAlpha.200"
          rounded="md"
          p={1}
          onClick={(event) => event.stopPropagation()}
        >
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              setServerQuiet(
                contextMenu.serverId,
                !quiet[contextMenu.serverId]
              );
              setContextMenu(undefined);
            }}
          >
            {quiet[contextMenu.serverId] ? "开启消息提醒" : "消息免打扰"}
          </Button>
        </Box>
      )}
    </Flex>
  );
}
