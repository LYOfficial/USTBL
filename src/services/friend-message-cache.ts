import { VustbFriendMessage } from "@/models/vustb";

const CACHE_PREFIX = "ustbl.friend-message-cache.v1";
const CACHE_LIMIT = 1000;

function cacheKey(userId: number, friendId: number) {
  const first = Math.min(userId, friendId);
  const second = Math.max(userId, friendId);
  return `${CACHE_PREFIX}.${first}.${second}`;
}

function canUseStorage() {
  return typeof window !== "undefined" && !!window.localStorage;
}

export function readFriendMessages(
  userId: number | undefined,
  friendId: number
): VustbFriendMessage[] {
  if (!userId || !canUseStorage()) return [];
  try {
    const value = JSON.parse(
      window.localStorage.getItem(cacheKey(userId, friendId)) || "[]"
    );
    if (!Array.isArray(value)) return [];
    return value.filter(isFriendMessage).slice(-CACHE_LIMIT);
  } catch {
    return [];
  }
}

export function appendFriendMessage(
  userId: number | undefined,
  friendId: number,
  message: VustbFriendMessage
) {
  if (!userId || !canUseStorage()) return;
  const messages = readFriendMessages(userId, friendId);
  if (messages.some((item) => item.id === message.id)) return;
  window.localStorage.setItem(
    cacheKey(userId, friendId),
    JSON.stringify([...messages, message].slice(-CACHE_LIMIT))
  );
}

export function clearFriendMessages(
  userId: number | undefined,
  friendId: number
) {
  if (!canUseStorage()) return;
  if (userId) {
    window.localStorage.removeItem(cacheKey(userId, friendId));
    return;
  }
  const prefix = `${CACHE_PREFIX}.`;
  for (let index = 0; index < window.localStorage.length; index += 1) {
    const key = window.localStorage.key(index);
    if (!key?.startsWith(prefix)) continue;
    const [, first, second] = key.slice(prefix.length).split(".");
    if (Number(first) === friendId || Number(second) === friendId) {
      window.localStorage.removeItem(key);
    }
  }
}

export function isFriendMessage(value: unknown): value is VustbFriendMessage {
  if (!value || typeof value !== "object") return false;
  const item = value as Partial<VustbFriendMessage>;
  return (
    typeof item.id === "string" &&
    typeof item.sender_id === "number" &&
    typeof item.recipient_id === "number" &&
    typeof item.sender === "string" &&
    typeof item.content === "string" &&
    typeof item.created_at === "string"
  );
}
