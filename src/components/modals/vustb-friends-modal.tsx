import {
  Avatar,
  Badge,
  Box,
  Button,
  Center,
  HStack,
  Icon,
  IconButton,
  Modal,
  ModalBody,
  ModalCloseButton,
  ModalContent,
  ModalFooter,
  ModalHeader,
  ModalOverlay,
  Spinner,
  Text,
  VStack,
} from "@chakra-ui/react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useCallback, useEffect, useState } from "react";
import { LuExternalLink, LuRefreshCw, LuUsersRound } from "react-icons/lu";
import { VustbFriend } from "@/models/vustb";
import { AccountService } from "@/services/account";

interface VustbFriendsModalProps {
  isOpen: boolean;
  onClose: () => void;
}

const VustbFriendsModal = ({ isOpen, onClose }: VustbFriendsModalProps) => {
  const [friends, setFriends] = useState<VustbFriend[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState("");

  const loadFriends = useCallback(async () => {
    setIsLoading(true);
    setError("");
    const accountResponse = await AccountService.retrieveVustbAccount();
    if (accountResponse.status !== "success" || !accountResponse.data) {
      setFriends([]);
      setError("请先登录像素北科账号后查看好友列表。");
      setIsLoading(false);
      return;
    }
    const response = await AccountService.retrieveVustbFriends();
    if (response.status === "success") {
      setFriends(
        [...response.data].sort(
          (left, right) =>
            Number(right.online) - Number(left.online) ||
            left.displayName.localeCompare(right.displayName, "zh-CN")
        )
      );
    } else {
      setFriends([]);
      setError(
        String(response.raw_error) === "EXPIRED"
          ? "像素北科登录已过期，请先同步账户资料并重新授权。"
          : response.details || "好友列表加载失败，请稍后重试。"
      );
    }
    setIsLoading(false);
  }, []);

  useEffect(() => {
    if (!isOpen) return;
    loadFriends();
    const timer = window.setInterval(loadFriends, 30_000);
    return () => window.clearInterval(timer);
  }, [isOpen, loadFriends]);

  return (
    <Modal isOpen={isOpen} onClose={onClose} size="lg" isCentered>
      <ModalOverlay />
      <ModalContent>
        <ModalHeader>
          <HStack spacing={2}>
            <Icon as={LuUsersRound} />
            <Text>好友列表</Text>
          </HStack>
        </ModalHeader>
        <ModalCloseButton />
        <ModalBody minH="240px" maxH="60vh" overflowY="auto">
          {isLoading && friends.length === 0 ? (
            <Center minH="220px">
              <Spinner />
            </Center>
          ) : error ? (
            <Center minH="220px" textAlign="center">
              <Text className="secondary-text">{error}</Text>
            </Center>
          ) : friends.length === 0 ? (
            <Center minH="220px" textAlign="center">
              <VStack spacing={2}>
                <Icon
                  as={LuUsersRound}
                  boxSize={8}
                  className="secondary-text"
                />
                <Text className="secondary-text">还没有好友</Text>
                <Text fontSize="sm" className="secondary-text">
                  可在像素北科用户主页添加好友。
                </Text>
              </VStack>
            </Center>
          ) : (
            <VStack align="stretch" spacing={2}>
              {friends.map((friend) => (
                <HStack
                  key={friend.id}
                  p={3}
                  spacing={3}
                  borderWidth="1px"
                  borderRadius="md"
                  borderColor="blackAlpha.200"
                  _dark={{ borderColor: "whiteAlpha.300" }}
                >
                  <Box position="relative" flexShrink={0}>
                    <Avatar
                      src={friend.avatarUrl}
                      name={friend.displayName}
                      boxSize="44px"
                      borderRadius="sm"
                    />
                    <Box
                      position="absolute"
                      right="-2px"
                      bottom="-2px"
                      boxSize="11px"
                      borderRadius="full"
                      bg={friend.online ? "green.400" : "gray.400"}
                      borderWidth="2px"
                      borderColor="chakra-body-bg"
                    />
                  </Box>
                  <Box minW={0} flex={1}>
                    <HStack spacing={2}>
                      <Text fontWeight="semibold" className="ellipsis-text">
                        {friend.displayName}
                      </Text>
                      <Badge colorScheme={friend.online ? "green" : "gray"}>
                        {friend.online ? "在线" : "离线"}
                      </Badge>
                    </HStack>
                    <Text fontSize="xs" className="secondary-text">
                      @{friend.username}
                    </Text>
                    {friend.online && friend.instanceName && (
                      <Text fontSize="sm" color="green.300" mt={1}>
                        正在游玩：{friend.instanceName}
                      </Text>
                    )}
                  </Box>
                  <IconButton
                    aria-label={`查看 ${friend.displayName} 的个人主页`}
                    title="查看个人主页"
                    size="sm"
                    variant="ghost"
                    icon={<LuExternalLink />}
                    onClick={() =>
                      openUrl(
                        `https://www.ustb.world/users/${encodeURIComponent(friend.username)}`
                      )
                    }
                  />
                </HStack>
              ))}
            </VStack>
          )}
        </ModalBody>
        <ModalFooter>
          <Button
            leftIcon={<LuRefreshCw />}
            variant="ghost"
            onClick={loadFriends}
            isLoading={isLoading}
          >
            刷新
          </Button>
          <Button ml={2} onClick={onClose}>
            关闭
          </Button>
        </ModalFooter>
      </ModalContent>
    </Modal>
  );
};

export default VustbFriendsModal;
