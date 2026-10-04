import { Center, Text } from "@chakra-ui/react";
import { useEffect, useState } from "react";
import MarkdownContainer from "@/components/common/markdown-container";
import { Section } from "@/components/common/section";
import { VustbAnnouncement } from "@/models/vustb";
import { AccountService } from "@/services/account";

export default function MessagesAnnouncementsPage() {
  const [items, setItems] = useState<VustbAnnouncement[]>([]);
  const [loading, setLoading] = useState(true);
  useEffect(() => { void AccountService.retrieveVustbAnnouncements().then((response) => { if (response.status === "success") setItems(response.data); setLoading(false); }); }, []);
  return <Section title="公告" display="flex" flexDirection="column" height="100%"><Center>{loading ? <Text>加载中…</Text> : null}</Center>{!loading && items.length === 0 ? <Center flex={1}><Text className="secondary-text">暂无公告</Text></Center> : items.map((item) => <article key={item.id} style={{ padding: 16, borderBottom: "1px solid var(--chakra-colors-blackAlpha-200)" }}><Text fontSize="lg" fontWeight="600" mb={2}>{item.title}</Text><MarkdownContainer>{item.content}</MarkdownContainer><Text fontSize="xs" className="secondary-text" mt={3}>{new Date(item.starts_at).toLocaleString()}</Text></article>)}</Section>;
}
