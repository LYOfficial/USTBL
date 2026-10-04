import { Box, Center, Flex, List, ListItem, Text } from "@chakra-ui/react";
import { useEffect, useMemo, useState } from "react";
import MarkdownContainer from "@/components/common/markdown-container";
import { VustbAnnouncement } from "@/models/vustb";
import { AccountService } from "@/services/account";

export default function MessagesAnnouncementsPage() {
  const [items, setItems] = useState<VustbAnnouncement[]>([]);
  const [selectedId, setSelectedId] = useState<number>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();

  useEffect(() => {
    let disposed = false;
    void AccountService.retrieveVustbAnnouncements().then((response) => {
      if (disposed) return;
      if (response.status === "success") {
        const next = [...response.data].sort((a, b) => Date.parse(b.starts_at) - Date.parse(a.starts_at));
        setItems(next);
        setSelectedId((current) => next.some((item) => item.id === current) ? current : next[0]?.id);
        setError(undefined);
      } else {
        setError(response.details || response.message || "公告加载失败");
      }
      setLoading(false);
    }).catch((reason) => {
      if (!disposed) { setError(String(reason)); setLoading(false); }
    });
    return () => { disposed = true; };
  }, []);

  const selected = useMemo(() => items.find((item) => item.id === selectedId), [items, selectedId]);

  return (
    <Flex h="100%" minH={0} border="1px solid" borderColor="blackAlpha.200" rounded="md" overflow="hidden">
      <Box w={{ base: "42%", md: "30%" }} minW={0} borderRight="1px solid" borderColor="blackAlpha.200" overflowY="auto">
        {loading ? <Center p={6}><Text>加载中…</Text></Center> : error ? <Center p={6}><Text color="red.500">{error}</Text></Center> : items.length === 0 ? <Center p={6}><Text className="secondary-text">暂无公告</Text></Center> : (
          <List>
            {items.map((item) => (
              <ListItem key={item.id} px={4} py={3} cursor="pointer" bg={item.id === selectedId ? "blackAlpha.100" : undefined} _hover={{ bg: "blackAlpha.50" }} onClick={() => setSelectedId(item.id)}>
                <Text fontWeight="600" noOfLines={2}>{item.title}</Text>
                <Text fontSize="xs" className="secondary-text" mt={1}>{new Date(item.starts_at).toLocaleDateString()}</Text>
              </ListItem>
            ))}
          </List>
        )}
      </Box>
      <Box flex={1} minW={0} overflowY="auto" p={{ base: 4, md: 6 }}>
        {selected ? <><Text fontSize="xl" fontWeight="700">{selected.title}</Text><Text fontSize="xs" className="secondary-text" mt={1} mb={5}>{new Date(selected.starts_at).toLocaleString()}</Text><MarkdownContainer>{selected.content}</MarkdownContainer></> : <Center h="100%"><Text className="secondary-text">选择一条公告查看详情</Text></Center>}
      </Box>
    </Flex>
  );
}
