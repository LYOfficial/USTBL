use crate::discover::models::{NewsPostResponse, NewsPostSummary, NewsSourceInfo};
use chrono::{DateTime, Utc};
use quick_xml::de::from_str;
use reqwest_middleware::ClientWithMiddleware;
use serde::Deserialize;

const RSS_DEFAULT_PAGE_SIZE: usize = 12;
const USTB_RSS_ENDPOINT: &str = "https://docs.ustb.world/api/rss?lang=zh";
const USTB_RSS_NAME: &str = "元宇宙体素工作坊";
const USTB_RSS_FULL_NAME: &str = "北京科技大学元宇宙体素工作坊";
const USTB_RSS_ICON: &str = "/images/icons/Logo_128x128.png";

#[derive(Debug, Default, Clone, Deserialize)]
struct RssFeed {
  #[serde(default)]
  channel: RssChannel,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct RssChannel {
  #[serde(default)]
  title: String,
  #[serde(default)]
  description: String,
  #[serde(default)]
  #[serde(rename = "item")]
  items: Vec<RssItem>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct RssItem {
  #[serde(default)]
  title: String,
  #[serde(default)]
  link: String,
  #[serde(default)]
  description: String,
  #[serde(default)]
  #[serde(rename = "pubDate")]
  pub_date: String,
  #[serde(default)]
  guid: String,
  #[serde(default)]
  category: String,
}

pub fn is_rss_source(url: &str) -> bool {
  let url = url.to_ascii_lowercase();
  url.contains("rss") || url.contains("feed") || url.ends_with(".xml")
}

fn parse_rss_date(value: &str) -> String {
  DateTime::parse_from_rfc2822(value)
    .map(|v| v.with_timezone(&Utc).to_rfc3339())
    .or_else(|_| DateTime::parse_from_rfc3339(value).map(|v| v.with_timezone(&Utc).to_rfc3339()))
    .unwrap_or_else(|_| Utc::now().to_rfc3339())
}

fn normalize_source_info(url: &str, feed: &RssFeed) -> NewsSourceInfo {
  if url == USTB_RSS_ENDPOINT {
    return NewsSourceInfo {
      name: USTB_RSS_NAME.to_string(),
      full_name: USTB_RSS_FULL_NAME.to_string(),
      endpoint_url: url.to_string(),
      icon_src: USTB_RSS_ICON.to_string(),
    };
  }

  let default_name = if feed.channel.title.trim().is_empty() {
    "RSS"
  } else {
    feed.channel.title.trim()
  };

  NewsSourceInfo {
    name: default_name.to_string(),
    full_name: if feed.channel.description.trim().is_empty() {
      default_name.to_string()
    } else {
      feed.channel.description.trim().to_string()
    },
    endpoint_url: url.to_string(),
    icon_src: String::new(),
  }
}

fn to_news_post(item: RssItem, source: NewsSourceInfo) -> Option<NewsPostSummary> {
  let link = if item.link.trim().is_empty() {
    item.guid.trim().to_string()
  } else {
    item.link.trim().to_string()
  };

  if link.is_empty() {
    return None;
  }

  Some(NewsPostSummary {
    title: item.title,
    abstracts: item.description,
    keywords: item.category,
    image_src: (String::new(), 0, 0),
    source,
    create_at: parse_rss_date(&item.pub_date),
    link,
  })
}

async fn fetch_rss_feed(client: &ClientWithMiddleware, url: &str) -> Option<RssFeed> {
  let response = client.get(url).send().await.ok()?;
  if !response.status().is_success() {
    return None;
  }

  let body = response.text().await.ok()?;
  from_str::<RssFeed>(&body).ok()
}

pub async fn fetch_rss_source_info(
  client: &ClientWithMiddleware,
  url: &str,
) -> Option<NewsSourceInfo> {
  let feed = fetch_rss_feed(client, url).await?;
  Some(normalize_source_info(url, &feed))
}

pub async fn fetch_rss_page(
  client: &ClientWithMiddleware,
  url: &str,
  cursor: Option<u64>,
) -> Option<(String, NewsPostResponse)> {
  let feed = fetch_rss_feed(client, url).await?;
  let source_info = normalize_source_info(url, &feed);

  let page = cursor.unwrap_or(1).max(1) as usize;
  let start = (page - 1) * RSS_DEFAULT_PAGE_SIZE;
  let total_items = feed.channel.items.len();

  if start >= total_items {
    return Some((
      url.to_string(),
      NewsPostResponse {
        posts: vec![],
        next: None,
        cursors: None,
      },
    ));
  }

  let posts = feed
    .channel
    .items
    .into_iter()
    .skip(start)
    .take(RSS_DEFAULT_PAGE_SIZE)
    .filter_map(|item| to_news_post(item, source_info.clone()))
    .collect::<Vec<_>>();

  let has_more = start + RSS_DEFAULT_PAGE_SIZE < total_items;

  Some((
    url.to_string(),
    NewsPostResponse {
      posts,
      next: if has_more {
        Some((page + 1) as u64)
      } else {
        None
      },
      cursors: None,
    },
  ))
}
