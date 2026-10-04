import { Box, Button, Flex, HStack, Input, Text, VStack } from "@chakra-ui/react";
import { useEffect, useRef, useState } from "react";
import { Section } from "@/components/common/section";
import { AccountService } from "@/services/account";
import { VustbFriend, VustbFriendMessage } from "@/models/vustb";

export default function MessagesFriendsPage() {
  const [friends, setFriends] = useState<VustbFriend[]>([]);
  const [selected, setSelected] = useState<VustbFriend>();
  const [messages, setMessages] = useState<VustbFriendMessage[]>([]);
  const [content, setContent] = useState("");
  const [quiet, setQuiet] = useState<Record<number, boolean>>({});
  const selectedId = useRef<number | undefined>(undefined);
  const load = async () => { const response = await AccountService.retrieveVustbFriends(); if (response.status === "success") { setFriends(response.data); setSelected((current) => { const next = current ? response.data.find((friend) => friend.id === current.id) : response.data[0]; return next ?? current; }); } };
  useEffect(() => { void load(); const timer = window.setInterval(() => void load(), 5000); return () => window.clearInterval(timer); }, []);
  useEffect(() => {
    void AccountService.startVustbFriendMessageStream();
    const unlisten = AccountService.onVustbFriendMessage((message) => {
      if (selectedId.current === message.sender_id || selectedId.current === message.recipient_id) {
        setMessages((current) => current.some((item) => item.id === message.id) ? current : [...current, message]);
      }
    });
    return unlisten;
  }, []);
  useEffect(() => {
    selectedId.current = selected?.id;
    setMessages([]);
  }, [selected?.id]);
  const send = async () => { if (!selected?.online || !content.trim()) return; const response = await AccountService.sendVustbFriendMessage(selected.id, content.trim()); if (response.status === "success") setContent(""); };
  useEffect(() => { setQuiet(Object.fromEntries(friends.map((friend) => [friend.id, window.localStorage.getItem(`ustbl.message.quiet.friend.${friend.id}`) === "true"]))); }, [friends]);
  return <Section title="好友消息" display="flex" flexDirection="column" height="100%"><Flex flex={1} minH={0} gap={3}><VStack align="stretch" w="32%" overflowY="auto">{friends.map((friend) => <Button key={friend.id} variant={friend.id === selected?.id ? "solid" : "ghost"} justifyContent="space-between" onClick={() => setSelected(friend)}><Text>{friend.displayName}</Text><Text fontSize="xs" color={friend.online ? "green.400" : "gray.400"} onClick={(event) => { event.stopPropagation(); const next = !quiet[friend.id]; setQuiet((current) => ({ ...current, [friend.id]: next })); window.localStorage.setItem(`ustbl.message.quiet.friend.${friend.id}`, String(next)); }}>{quiet[friend.id] ? "提醒关" : friend.online ? "在线" : "离线"}</Text></Button>)}</VStack><Flex direction="column" flex={1}><Box flex={1} overflowY="auto" p={2}>{messages.map((message) => <Box key={String(message.id)} p={3} mb={2} rounded="md" bg="blackAlpha.50"><Text fontWeight="600">{String(message.sender || "好友")}</Text><Text>{String(message.content || "")}</Text></Box>)}</Box><HStack pt={2}><Input value={content} onChange={(event) => setContent(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") void send(); }} placeholder={selected?.online ? "输入纯文本消息" : "好友不在线，无法对话"} isDisabled={!selected?.online} /><Button colorScheme="blue" onClick={() => void send()} isDisabled={!selected?.online}>发送</Button></HStack></Flex></Flex></Section>;
}
