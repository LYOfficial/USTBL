import {
  Avatar,
  Badge,
  Box,
  Button,
  Flex,
  HStack,
  Input,
  Text,
  VStack,
} from "@chakra-ui/react";
import { useEffect, useMemo, useRef, useState } from "react";
import { VustbFriend, VustbFriendMessage } from "@/models/vustb";
import { AccountService } from "@/services/account";

export default function MessagesFriendsPage() {
  const [friends, setFriends] = useState<VustbFriend[]>([]);
  const [selected, setSelected] = useState<VustbFriend>();
  const [messages, setMessages] = useState<VustbFriendMessage[]>([]);
  const [content, setContent] = useState("");
  const [quiet, setQuiet] = useState<Record<number, boolean>>({});
  const [contextMenu, setContextMenu] = useState<{
    friendId: number;
    x: number;
    y: number;
  }>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const selectedId = useRef<number>();
  const nextRetryAt = useRef(0);

  const load = async () => {
    if (Date.now() < nextRetryAt.current) return;
    setLoading(true);
    const response = await AccountService.retrieveVustbFriends();
    if (response.status === "success") {
      const sorted = [...response.data].sort(
        (a, b) =>
          Number(b.online) - Number(a.online) ||
          a.displayName.localeCompare(b.displayName)
      );
      setFriends(sorted);
      setSelected(
        (current) =>
          sorted.find((friend) => friend.id === current?.id) ?? sorted[0]
      );
      setError(undefined);
      nextRetryAt.current = 0;
    } else {
      setError(response.details || response.message || "好友列表加载失败");
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
    void AccountService.startVustbFriendMessageStream();
    const unlisten = AccountService.onVustbFriendMessage((message) => {
      if (
        selectedId.current === message.sender_id ||
        selectedId.current === message.recipient_id
      ) {
        setMessages((current) =>
          current.some((item) => item.id === message.id)
            ? current
            : [...current, message]
        );
      }
    });
    return unlisten;
  }, []);

  useEffect(() => {
    selectedId.current = selected?.id;
    setMessages([]);
  }, [selected?.id]);

  useEffect(() => {
    setQuiet(
      Object.fromEntries(
        friends.map((friend) => [
          friend.id,
          window.localStorage.getItem(
            `ustbl.message.quiet.friend.${friend.id}`
          ) === "true",
        ])
      )
    );
  }, [friends]);

  const send = async () => {
    if (!selected?.online || !content.trim()) return;
    const response = await AccountService.sendVustbFriendMessage(
      selected.id,
      content.trim()
    );
    if (response.status === "success") setContent("");
  };
  const visibleMessages = useMemo(() => messages, [messages]);

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
        {loading && friends.length === 0 ? (
          <Text p={4} className="secondary-text">
            加载中…
          </Text>
        ) : error && friends.length === 0 ? (
          <Text p={4} color="red.500">
            {error}
          </Text>
        ) : friends.length === 0 ? (
          <Text p={4} className="secondary-text">
            暂无好友
          </Text>
        ) : (
          friends.map((friend) => (
            <Button
              key={friend.id}
              minH="68px"
              py={3}
              variant="ghost"
              bg={
                friend.id === selected?.id
                  ? "blackAlpha.100"
                  : friend.online
                    ? "green.50"
                    : undefined
              }
              border="1px solid"
              borderColor={
                friend.id === selected?.id ? "blue.400" : "transparent"
              }
              _hover={{
                bg:
                  friend.id === selected?.id
                    ? "blackAlpha.100"
                    : "blackAlpha.50",
              }}
              borderRadius="md"
              justifyContent="flex-start"
              textAlign="left"
              onClick={() => setSelected(friend)}
              onContextMenu={(event) => {
                event.preventDefault();
                setSelected(friend);
                setContextMenu({
                  friendId: friend.id,
                  x: event.clientX,
                  y: event.clientY,
                });
              }}
            >
              <Avatar
                size="sm"
                src={friend.avatarUrl}
                name={friend.displayName}
                mr={2}
              />
              <Box flex={1} minW={0}>
                <Text noOfLines={1}>{friend.displayName}</Text>
                <Text
                  fontSize="xs"
                  color={friend.online ? "green.500" : "gray.500"}
                >
                  {friend.online ? "在线" : "离线"}
                  {quiet[friend.id] ? " · 免打扰" : ""}
                </Text>
              </Box>
              {friend.online && <Badge colorScheme="green">在线</Badge>}
            </Button>
          ))
        )}
      </VStack>
      <Flex direction="column" flex={1} minW={0}>
        <Box flex={1} overflowY="auto" p={3}>
          {visibleMessages.map((message) => (
            <Box
              key={String(message.id)}
              p={2}
              mb={2}
              rounded="md"
              bg="blackAlpha.50"
            >
              <HStack justify="space-between" spacing={2}>
                <Text fontWeight="600" fontSize="sm" noOfLines={1}>
                  {String(message.sender || "好友")}
                </Text>
                <Text fontSize="xs" className="secondary-text" flexShrink={0}>
                  {message.created_at
                    ? new Date(message.created_at).toLocaleTimeString()
                    : ""}
                </Text>
              </HStack>
              <Text mt={1} fontSize="sm" whiteSpace="pre-wrap">
                {String(message.content || "")}
              </Text>
            </Box>
          ))}
        </Box>
        <HStack p={3} borderTop="1px solid" borderColor="blackAlpha.200">
          <Input
            value={content}
            onChange={(event) => setContent(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void send();
            }}
            placeholder={
              selected?.online ? "输入纯文本消息" : "好友不在线，无法对话"
            }
            isDisabled={!selected?.online}
          />
          <Button
            colorScheme="blue"
            onClick={() => void send()}
            isDisabled={!selected?.online}
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
              const value = !quiet[contextMenu.friendId];
              setQuiet((current) => ({
                ...current,
                [contextMenu.friendId]: value,
              }));
              window.localStorage.setItem(
                `ustbl.message.quiet.friend.${contextMenu.friendId}`,
                String(value)
              );
              setContextMenu(undefined);
            }}
          >
            {quiet[contextMenu.friendId] ? "开启好友提醒" : "好友消息免打扰"}
          </Button>
        </Box>
      )}
    </Flex>
  );
}
