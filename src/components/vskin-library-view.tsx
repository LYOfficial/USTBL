import {
  Badge,
  Box,
  Button,
  Grid,
  HStack,
  Skeleton,
  Text,
  VStack,
  useDisclosure,
} from "@chakra-ui/react";
import { useCallback, useEffect, useState } from "react";
import SegmentedControl from "@/components/common/segmented";
import VskinTextureModal from "@/components/modals/vskin-texture-modal";
import VskinTextureThumbnail from "@/components/vskin-texture-thumbnail";
import { useToast } from "@/contexts/toast";
import { VustbTexture } from "@/models/vustb";
import { AccountService } from "@/services/account";

const PAGE_SIZE = 20;

const VskinLibraryView: React.FC = () => {
  const toast = useToast();
  const [items, setItems] = useState<VustbTexture[]>([]);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [filter, setFilter] = useState<"all" | "skin" | "cape">("all");
  const [isLoading, setIsLoading] = useState(true);
  const [selectedTexture, setSelectedTexture] = useState<VustbTexture>();
  const modal = useDisclosure();

  const load = useCallback(async () => {
    setIsLoading(true);
    const response = await AccountService.retrieveVustbSkinLibrary(
      page,
      PAGE_SIZE,
      filter === "all" ? undefined : filter
    );
    if (response.status === "success") {
      setItems(response.data.items);
      setTotal(response.data.total);
    } else {
      setItems([]);
      setTotal(0);
      toast({
        title: response.details || response.message,
        status: "error",
      });
    }
    setIsLoading(false);
  }, [filter, page, toast]);

  useEffect(() => {
    load();
  }, [load]);

  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE));

  const handleCollected = useCallback((hash: string) => {
    setItems((current) =>
      current.map((item) =>
        item.hash === hash ? { ...item, collected: true } : item
      )
    );
    setSelectedTexture((current) =>
      current?.hash === hash ? { ...current, collected: true } : current
    );
  }, []);

  return (
    <VStack align="stretch" spacing={4} h="100%" minH={0}>
      <HStack justify="space-between">
        <Text fontSize="sm" color="gray.500">
          共 {total} 个材质
        </Text>
        <SegmentedControl
          size="sm"
          selected={filter}
          items={[
            { value: "all", label: "全部" },
            { value: "skin", label: "皮肤" },
            { value: "cape", label: "披风" },
          ]}
          onSelectItem={(value) => {
            setFilter(value as "all" | "skin" | "cape");
            setPage(1);
          }}
        />
      </HStack>
      <Box flex="1" minH={0} overflowY="auto" pr={1}>
        <Grid templateColumns="repeat(auto-fill, minmax(150px, 1fr))" gap={3}>
          {(isLoading ? Array.from({ length: 8 }) : items).map((item, index) =>
            isLoading ? (
              <Skeleton key={index} h="210px" rounded="lg" />
            ) : (
              <Box
                key={(item as VustbTexture).hash}
                borderWidth="1px"
                rounded="lg"
                overflow="hidden"
                cursor="pointer"
                transition="all 0.16s ease"
                _hover={{
                  transform: "translateY(-2px)",
                  boxShadow: "md",
                  borderColor: "blue.300",
                }}
                onClick={() => {
                  setSelectedTexture(item as VustbTexture);
                  modal.onOpen();
                }}
              >
                <VskinTextureThumbnail
                  texture={item as VustbTexture}
                  aspectRatio={1}
                />
                <VStack align="stretch" spacing={1} p={3}>
                  <HStack justify="space-between" minW={0}>
                    <Text fontWeight="semibold" fontSize="sm" noOfLines={1}>
                      {(item as VustbTexture).name || "未命名材质"}
                    </Text>
                    {(item as VustbTexture).collected && (
                      <Badge colorScheme="blue">已收藏</Badge>
                    )}
                  </HStack>
                  <Text fontSize="xs" color="gray.500" noOfLines={1}>
                    {(item as VustbTexture).uploaderName || "未知上传者"} ·{" "}
                    {(item as VustbTexture).type === "skin"
                      ? (item as VustbTexture).model
                      : "披风"}
                  </Text>
                </VStack>
              </Box>
            )
          )}
        </Grid>
        {!isLoading && items.length === 0 && (
          <Text py={12} textAlign="center" color="gray.500">
            皮肤库暂时是空的
          </Text>
        )}
      </Box>
      <HStack justify="center">
        <Button
          size="sm"
          variant="ghost"
          isDisabled={page <= 1}
          onClick={() => setPage((value) => value - 1)}
        >
          上一页
        </Button>
        <Text fontSize="sm">
          {page} / {totalPages}
        </Text>
        <Button
          size="sm"
          variant="ghost"
          isDisabled={page >= totalPages}
          onClick={() => setPage((value) => value + 1)}
        >
          下一页
        </Button>
      </HStack>
      <VskinTextureModal
        texture={selectedTexture}
        isOpen={modal.isOpen}
        onClose={modal.onClose}
        onCollected={handleCollected}
      />
    </VStack>
  );
};

export default VskinLibraryView;
