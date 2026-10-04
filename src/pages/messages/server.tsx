import { Box, Button, Flex, HStack, Input, Text, VStack } from "@chakra-ui/react";
import { useEffect, useMemo, useState } from "react";
import { Section } from "@/components/common/section";
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
  const load = async () => { const response = await AccountService.retrieveVustbServerMessages(); if (response.status === "success") { setGroups(response.data); setSelectedId((current) => current ?? response.data[0]?.id); } };
  useEffect(() => { void load(); const timer = window.setInterval(() => void load(), 5000); return () => window.clearInterval(timer); }, []);
  useEffect(() => { setQuiet(Object.fromEntries(groups.map((group) => [group.id, window.localStorage.getItem(`ustbl.message.quiet.server.${group.id}`) === "true"]))); }, [groups]);
  const selected = useMemo(() => groups.find((group) => group.id === selectedId), [groups, selectedId]);
  const send = async () => { if (!selected || !selectedPlayer?.name || !content.trim()) return; const response = await AccountService.sendVustbServerMessage(selected.id, selectedPlayer.name, content.trim()); if (response.status === "success") { setContent(""); void load(); } else toast({ title: response.message, description: response.details, status: "error" }); };
  return <Section title="服务器消息" display="flex" flexDirection="column" height="100%"><Flex flex={1} minH={0} gap={3}><VStack align="stretch" w="32%" overflowY="auto">{groups.map((group) => <Button key={group.id} variant={group.id === selectedId ? "solid" : "ghost"} justifyContent="space-between" onClick={() => setSelectedId(group.id)}><Text>{group.name}</Text><Text fontSize="xs" onClick={(event) => { event.stopPropagation(); const next = !quiet[group.id]; setQuiet((current) => ({ ...current, [group.id]: next })); window.localStorage.setItem(`ustbl.message.quiet.server.${group.id}`, String(next)); }}>{quiet[group.id] ? "提醒关" : "提醒开"}</Text></Button>)}</VStack><Flex direction="column" flex={1} minW={0}><Box flex={1} overflowY="auto" p={2}>{selected?.messages.map((message: VustbServerMessage) => <Box key={message.id} mb={3} p={3} rounded="md" bg={message.sourceType === "ustbl" ? "blackAlpha.100" : "blue.50"}><HStack justify="space-between"><Text fontWeight="600">{message.sender}</Text><Text fontSize="xs" className="secondary-text">{new Date(message.createdAt).toLocaleTimeString()}</Text></HStack><Text whiteSpace="pre-wrap">{message.content}</Text></Box>)}</Box><HStack pt={2}><Input value={content} onChange={(event) => setContent(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") void send(); }} placeholder={selectedPlayer?.name ? "输入纯文本消息" : "请选择像素北科游戏角色后发送"} isDisabled={!selectedPlayer?.name || !selected} /><Button colorScheme="blue" onClick={() => void send()} isDisabled={!selectedPlayer?.name || !selected}>发送</Button></HStack></Flex></Flex></Section>;
}
