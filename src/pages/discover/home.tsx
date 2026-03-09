import {
  Avatar,
  Card,
  HStack,
  SimpleGrid,
  Text,
  VStack,
} from "@chakra-ui/react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useRouter } from "next/router";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { LuGlobe } from "react-icons/lu";
import { BeatLoader } from "react-spinners";
import { CommonIconButton } from "@/components/common/common-icon-button";
import Empty from "@/components/common/empty";
import { OptionItem } from "@/components/common/option-item";
import { Section } from "@/components/common/section";
import { useLauncherConfig } from "@/contexts/config";
import { NewsPostSummary } from "@/models/news-post";
import { MC_NEWS_SOURCE_URL } from "@/pages/discover/minecraft-news";
import { DiscoverService } from "@/services/discover";
import { formatRelativeTime } from "@/utils/datetime";

type NewsPanelProps = {
  title: string;
  posts: NewsPostSummary[];
  loading: boolean;
  onRefresh: () => void;
  onMore: () => void;
  accentColor: string;
};

const MAX_NEWS_POST_NUM = 6;

const NewsPanel: React.FC<NewsPanelProps> = ({
  title,
  posts,
  loading,
  onRefresh,
  onMore,
  accentColor,
}) => {
  const { t } = useTranslation();

  return (
    <Section
      title={title}
      headExtra={
        <HStack>
          <CommonIconButton
            icon="refresh"
            onClick={onRefresh}
            isDisabled={loading}
            size="xs"
            h={21}
          />
          <CommonIconButton icon="more" onClick={onMore} size="xs" h={21} />
        </HStack>
      }
    >
      {loading ? (
        <VStack py={6}>
          <BeatLoader size={14} color="gray" />
        </VStack>
      ) : posts.length === 0 ? (
        <Empty withIcon={false} size="sm" />
      ) : (
        <SimpleGrid columns={{ base: 1, lg: 2, xl: 3 }} gap={3}>
          {posts.map((post) => (
            <Card key={post.link}>
              <OptionItem
                title={post.title}
                titleLineWrap={false}
                description={
                  <Text fontSize="xs" className="secondary-text" noOfLines={3}>
                    {post.abstract}
                  </Text>
                }
                prefixElement={
                  <Avatar
                    name={post.source.name}
                    src={post.source.iconSrc}
                    boxSize={8}
                  />
                }
                isFullClickZone
                onClick={() => openUrl(post.link)}
              >
                <Text fontSize="xs" className="secondary-text">
                  {formatRelativeTime(post.createAt, t)}
                </Text>
              </OptionItem>
            </Card>
          ))}
        </SimpleGrid>
      )}
    </Section>
  );
};

export const DiscoverHomePage = () => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const router = useRouter();
  const primaryColor = config.appearance.theme.primaryColor;
  const accentColor = `var(--chakra-colors-${primaryColor}-400)`;

  const [communityPosts, setCommunityPosts] = useState<NewsPostSummary[]>([]);
  const [mcPosts, setMcPosts] = useState<NewsPostSummary[]>([]);
  const [isLoadingCommunity, setIsLoadingCommunity] = useState<boolean>(false);
  const [isLoadingMC, setIsLoadingMC] = useState<boolean>(false);

  const fetchCommunityNews = useCallback(async () => {
    setIsLoadingCommunity(true);
    try {
      const source = [
        { url: "https://docs.ustb.world/api/rss?lang=zh", cursor: null },
      ];
      const response = await DiscoverService.fetchNewsPostSummaries(source);
      if (response.status === "success") {
        setCommunityPosts(response.data.posts.slice(0, MAX_NEWS_POST_NUM));
      }
    } finally {
      setIsLoadingCommunity(false);
    }
  }, []);

  const fetchMinecraftNews = useCallback(async () => {
    setIsLoadingMC(true);
    try {
      const source = [{ url: MC_NEWS_SOURCE_URL, cursor: null }];
      const response = await DiscoverService.fetchNewsPostSummaries(source);
      if (response.status === "success") {
        setMcPosts(response.data.posts.slice(0, MAX_NEWS_POST_NUM));
      }
    } finally {
      setIsLoadingMC(false);
    }
  }, []);

  useEffect(() => {
    fetchCommunityNews();
    fetchMinecraftNews();
  }, [fetchCommunityNews, fetchMinecraftNews]);

  return (
    <Section px={{ base: 4, lg: 6 }}>
      <VStack align="stretch" spacing={6} pb={4}>
        <NewsPanel
          title={t("DiscoverHomePage.minecraft-news")}
          posts={mcPosts}
          loading={isLoadingMC}
          onRefresh={fetchMinecraftNews}
          onMore={() => router.push("/discover/minecraft-news")}
          accentColor={accentColor}
        />
        <NewsPanel
          title={t("DiscoverHomePage.community-news")}
          posts={communityPosts}
          loading={isLoadingCommunity}
          onRefresh={fetchCommunityNews}
          onMore={() => router.push("/discover/community-news")}
          accentColor={accentColor}
        />
      </VStack>
    </Section>
  );
};

export default DiscoverHomePage;
