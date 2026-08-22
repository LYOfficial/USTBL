import {
  Badge,
  Box,
  Button,
  Center,
  HStack,
  Icon,
  Modal,
  ModalBody,
  ModalContent,
  ModalFooter,
  ModalHeader,
  ModalOverlay,
  Progress,
  Radio,
  RadioGroup,
  Text,
  Tooltip,
  VStack,
} from "@chakra-ui/react";
import { open } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useMemo, useState } from "react";
import {
  LuArrowLeft,
  LuCloudDownload,
  LuFilePlus2,
  LuFolder,
  LuPencil,
  LuRefreshCw,
  LuRotateCw,
  LuX,
} from "react-icons/lu";
import { BeatLoader } from "react-spinners";
import Empty from "@/components/common/empty";
import { OptionItem, OptionItemGroup } from "@/components/common/option-item";
import { Section } from "@/components/common/section";
import { useGlobalData } from "@/contexts/global-data";
import { useToast } from "@/contexts/toast";
import { useTauriFileDrop } from "@/hooks/drag-and-drop";
import { InstanceSummary } from "@/models/instance/misc";
import {
  SharedFolder,
  SharedInstance,
  SharedInstanceDetail,
  SharedMod,
  SharedUpdateProgress,
  SharedUpdateResult,
} from "@/models/shared-instance";
import { VustbAccount } from "@/models/vustb";
import { AccountService } from "@/services/account";
import { SharedInstanceService } from "@/services/shared-instance";

const managerGroups = new Set([
  "super_admin",
  "admin",
  "platform_manager",
  "server_manager",
]);

type BindingDialogMode = "select" | "confirm" | null;

const folderPath = (
  folders: SharedFolder[],
  folderId: number | null
): string | null => {
  if (folderId === null) return "";

  const foldersById = new Map(folders.map((folder) => [folder.id, folder]));
  const names: string[] = [];
  const visited = new Set<number>();
  let currentFolderId: number | null = folderId;

  while (currentFolderId !== null) {
    if (visited.has(currentFolderId)) return null;
    visited.add(currentFolderId);
    const folder = foldersById.get(currentFolderId);
    if (!folder) return null;
    names.push(folder.name);
    currentFolderId = folder.parentId;
  }

  return names.reverse().join("/");
};

const sharedFilePath = (detail: SharedInstanceDetail, file: SharedMod) => {
  const parentPath = folderPath(detail.folders, file.folderId);
  return parentPath === null
    ? `未知目录/${file.fileName}`
    : parentPath
      ? `${parentPath}/${file.fileName}`
      : file.fileName;
};

const SharedInstancesPage = () => {
  const toast = useToast();
  const { getInstanceList } = useGlobalData();
  const [instances, setInstances] = useState<SharedInstance[]>([]);
  const [selected, setSelected] = useState<SharedInstanceDetail | null>(null);
  const [account, setAccount] = useState<VustbAccount | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [isSyncing, setIsSyncing] = useState(false);
  const [isUpdating, setIsUpdating] = useState(false);
  const [isEditing, setIsEditing] = useState(false);
  const [updateProgress, setUpdateProgress] =
    useState<SharedUpdateProgress | null>(null);
  const [updateResult, setUpdateResult] = useState<SharedUpdateResult | null>(
    null
  );
  const [bindingDialogMode, setBindingDialogMode] =
    useState<BindingDialogMode>(null);
  const [boundInstanceId, setBoundInstanceId] = useState("");
  const [chosenInstanceId, setChosenInstanceId] = useState("");
  const [currentFolderId, setCurrentFolderId] = useState<number | null>(null);

  const localInstances = useMemo(
    () => getInstanceList() || [],
    [getInstanceList]
  );
  const canManage = !!account && managerGroups.has(account.userGroup);

  const showError = useCallback(
    (response: { message: string; details?: string }) => {
      toast({
        title: response.message,
        description: response.details,
        status: "error",
      });
    },
    [toast]
  );

  const refreshList = useCallback(async () => {
    setIsLoading(true);
    const response = await SharedInstanceService.retrieveList();
    if (response.status === "success") {
      setInstances(response.data);
    } else {
      showError(response);
    }
    setIsLoading(false);
  }, [showError]);

  const refreshAccount = useCallback(async () => {
    const response = await AccountService.retrieveVustbAccount();
    if (response.status === "success") setAccount(response.data);
  }, []);

  useEffect(() => {
    refreshList();
    refreshAccount();
  }, [refreshAccount, refreshList]);

  useEffect(() => {
    return SharedInstanceService.onUpdateProgress((progress) => {
      if (progress.sharedInstanceId === selected?.id) {
        setUpdateProgress(progress);
      }
    });
  }, [selected?.id]);

  const openInstance = async (instance: SharedInstance) => {
    setIsLoading(true);
    const response = await SharedInstanceService.retrieveDetail(instance.id);
    if (response.status === "success") {
      setSelected(response.data);
      setIsEditing(false);
      setCurrentFolderId(null);
    } else {
      showError(response);
    }
    setIsLoading(false);
  };

  const syncSelected = useCallback(async () => {
    if (!selected) return;
    setIsSyncing(true);
    const response = await SharedInstanceService.retrieveDetail(selected.id);
    if (response.status === "success") {
      setSelected(response.data);
      if (
        currentFolderId !== null &&
        !response.data.folders.some((folder) => folder.id === currentFolderId)
      ) {
        setCurrentFolderId(null);
      }
      toast({ title: "共享实例文件列表已同步", status: "success" });
    } else {
      showError(response);
    }
    setIsSyncing(false);
  }, [currentFolderId, selected, showError, toast]);

  const runUpdate = useCallback(
    async (localInstanceId: string) => {
      if (!selected) return;
      setBindingDialogMode(null);
      setUpdateResult(null);
      setUpdateProgress({
        sharedInstanceId: selected.id,
        current: 0,
        total: selected.mods.filter((item) =>
          ["used", "deleted"].includes(item.status)
        ).length,
      });
      setIsUpdating(true);
      const response = await SharedInstanceService.update(
        selected.id,
        localInstanceId
      );
      if (response.status === "success") {
        setBoundInstanceId(localInstanceId);
        setUpdateResult(response.data);
      } else {
        showError(response);
      }
      setIsUpdating(false);
    },
    [selected, showError]
  );

  const requestUpdate = useCallback(async () => {
    if (!selected) return;
    const response = await SharedInstanceService.retrieveBinding(selected.id);
    if (response.status !== "success") {
      showError(response);
      return;
    }
    const binding = response.data || "";
    const usableBinding = localInstances.some((item) => item.id === binding)
      ? binding
      : "";
    setBoundInstanceId(usableBinding);
    setChosenInstanceId(usableBinding || localInstances[0]?.id || "");
    setBindingDialogMode(usableBinding ? "confirm" : "select");
  }, [localInstances, selected, showError]);

  const bindAndUpdate = async () => {
    if (!selected || !chosenInstanceId) return;
    const response = await SharedInstanceService.setBinding(
      selected.id,
      chosenInstanceId
    );
    if (response.status !== "success") {
      showError(response);
      return;
    }
    runUpdate(chosenInstanceId);
  };

  const uploadFile = useCallback(
    async (filePath: string) => {
      if (!selected || !isEditing || !canManage) return;
      const fileName = filePath.split(/[\\/]/).pop();
      if (!fileName) return;
      const existingFile = selected.mods.find(
        (file) => file.fileName === fileName
      );
      const response = existingFile
        ? await SharedInstanceService.updateMod(
            selected.id,
            existingFile.id,
            filePath
          )
        : await SharedInstanceService.uploadMod(
            selected.id,
            filePath,
            currentFolderId
          );
      if (response.status === "success") {
        toast({
          title: `${existingFile ? "已更新" : "已添加"} ${response.data.fileName}`,
          status: "success",
        });
        syncSelected();
      } else {
        showError(response);
      }
    },
    [
      canManage,
      isEditing,
      selected,
      showError,
      syncSelected,
      toast,
      currentFolderId,
    ]
  );

  useTauriFileDrop({ pattern: ".+", onMatch: uploadFile });

  const chooseFile = async () => {
    const selectedPath = await open({
      title: "选择要添加到共享实例的文件",
      multiple: false,
    });
    if (typeof selectedPath === "string") uploadFile(selectedPath);
  };

  const deleteMod = async (mod: SharedMod) => {
    if (!selected) return;
    if (
      !window.confirm(
        `删除 ${sharedFilePath(selected, mod)} 的共享档案不可恢复，确定继续吗？`
      )
    )
      return;
    const response = await SharedInstanceService.deleteMod(selected.id, mod.id);
    if (response.status === "success") {
      toast({
        title: `已删除 ${sharedFilePath(selected, mod)}`,
        status: "success",
      });
      syncSelected();
    } else {
      showError(response);
    }
  };

  const selectedLocalInstance = useMemo(
    () => localInstances.find((item) => item.id === boundInstanceId),
    [boundInstanceId, localInstances]
  );

  const currentFolder = useMemo(
    () =>
      selected?.folders.find((folder) => folder.id === currentFolderId) || null,
    [currentFolderId, selected]
  );
  const currentFolderPath = useMemo(
    () =>
      selected && currentFolder
        ? folderPath(selected.folders, currentFolder.id) || currentFolder.name
        : "",
    [currentFolder, selected]
  );
  const currentFolders = useMemo(
    () =>
      selected?.folders.filter(
        (folder) => folder.parentId === currentFolderId
      ) || [],
    [currentFolderId, selected]
  );
  const currentFiles = useMemo(
    () =>
      selected?.mods.filter((file) => file.folderId === currentFolderId) || [],
    [currentFolderId, selected]
  );

  const goBack = () => {
    if (currentFolder) {
      setCurrentFolderId(currentFolder.parentId);
    } else {
      setSelected(null);
    }
  };

  if (!selected) {
    return (
      <Section
        display="flex"
        flexDirection="column"
        height="100%"
        title="共享实例"
        description="浏览并同步像素北科维护的实例文件"
        headExtra={
          <Button
            size="xs"
            leftIcon={<LuRefreshCw />}
            onClick={refreshList}
            isLoading={isLoading}
          >
            同步
          </Button>
        }
      >
        <Box overflow="auto" flexGrow={1} rounded="md">
          {isLoading ? (
            <Center mt={8}>
              <BeatLoader size={14} color="gray" />
            </Center>
          ) : instances.length ? (
            <OptionItemGroup
              items={instances.map((instance) => ({
                title: instance.name,
                description: `最近更新：${new Date(instance.updatedAt).toLocaleString()}`,
                isFullClickZone: true,
                onClick: () => openInstance(instance),
                children: <Icon as={LuCloudDownload} color="blue.500" />,
              }))}
            />
          ) : (
            <Empty withIcon={false} size="sm" />
          )}
        </Box>
      </Section>
    );
  }

  return (
    <>
      <Section
        display="flex"
        flexDirection="column"
        height="100%"
        title={selected.name}
        description={
          currentFolderPath
            ? `共享实例 · ${currentFolderPath}`
            : `共享实例 · ${selected.mods.filter((item) => item.status === "used").length} 个使用中文件`
        }
        headExtra={
          <HStack spacing={2}>
            <Tooltip
              label={currentFolder ? "返回上一级目录" : "返回共享实例列表"}
            >
              <Button size="xs" variant="ghost" onClick={goBack}>
                <Icon as={LuArrowLeft} />
              </Button>
            </Tooltip>
            <Button
              size="xs"
              leftIcon={<LuRefreshCw />}
              onClick={syncSelected}
              isLoading={isSyncing}
            >
              同步
            </Button>
            <Button
              size="xs"
              colorScheme="blue"
              leftIcon={<LuRotateCw />}
              onClick={requestUpdate}
              isLoading={isUpdating}
            >
              更新
            </Button>
            {canManage && (
              <Button
                size="xs"
                variant={isEditing ? "solid" : "outline"}
                colorScheme="orange"
                leftIcon={<LuPencil />}
                onClick={() => setIsEditing((value) => !value)}
              >
                {isEditing ? "完成编辑" : "编辑"}
              </Button>
            )}
          </HStack>
        }
      >
        {isEditing && canManage && (
          <Box
            borderWidth="1px"
            borderStyle="dashed"
            borderColor="blue.300"
            borderRadius="md"
            px={3}
            py={2}
            mb={3}
          >
            <HStack justify="space-between">
              <Text fontSize="sm">拖入文件，或选择文件添加到当前目录。</Text>
              <Button size="xs" leftIcon={<LuFilePlus2 />} onClick={chooseFile}>
                添加文件
              </Button>
            </HStack>
          </Box>
        )}
        <Box overflow="auto" flexGrow={1} rounded="md">
          {currentFolders.length || currentFiles.length ? (
            <OptionItemGroup
              items={[
                ...currentFolders.map((folder) => (
                  <OptionItem
                    key={`folder-${folder.id}`}
                    prefixElement={<Icon as={LuFolder} color="blue.500" />}
                    title={folder.name}
                    description="共享文件夹"
                    titleExtra={<Badge colorScheme="blue">文件夹</Badge>}
                    isFullClickZone
                    onClick={() => setCurrentFolderId(folder.id)}
                  />
                )),
                ...currentFiles.map((mod) => (
                  <OptionItem
                    key={`file-${mod.id}`}
                    title={mod.fileName}
                    description={`${Math.max(0, mod.fileSize / 1024 / 1024).toFixed(2)} MiB · ${mod.createdByUsername || "未知上传者"}`}
                    titleExtra={
                      <Badge
                        colorScheme={mod.status === "used" ? "green" : "red"}
                      >
                        {mod.status === "used" ? "使用中" : "已删除"}
                      </Badge>
                    }
                  >
                    {isEditing && canManage && mod.status === "used" && (
                      <Tooltip label="删除共享文件">
                        <Button
                          size="xs"
                          variant="ghost"
                          colorScheme="red"
                          aria-label={`删除 ${sharedFilePath(selected, mod)}`}
                          onClick={() => deleteMod(mod)}
                        >
                          <Icon as={LuX} />
                        </Button>
                      </Tooltip>
                    )}
                  </OptionItem>
                )),
              ]}
            />
          ) : (
            <Empty withIcon={false} size="sm" />
          )}
        </Box>
      </Section>

      <Modal
        isOpen={isUpdating}
        onClose={() => undefined}
        isCentered
        closeOnEsc={false}
        closeOnOverlayClick={false}
      >
        <ModalOverlay />
        <ModalContent>
          <ModalHeader>正在同步共享实例文件</ModalHeader>
          <ModalBody pb={6}>
            <VStack spacing={4} align="stretch">
              <Text textAlign="center">
                已处理 {updateProgress?.current || 0} /{" "}
                {updateProgress?.total || 0} 个文件
              </Text>
              <Progress
                value={
                  updateProgress && updateProgress.total > 0
                    ? (updateProgress.current / updateProgress.total) * 100
                    : 0
                }
                colorScheme="blue"
                size="md"
                borderRadius="md"
              />
              <Text
                minH={5}
                fontSize="sm"
                textAlign="center"
                className={
                  updateProgress?.fileName
                    ? "secondary-text ellipsis-text"
                    : "secondary-text"
                }
              >
                {updateProgress?.fileName
                  ? `正在处理：${updateProgress.fileName}`
                  : "正在准备文件列表…"}
              </Text>
            </VStack>
          </ModalBody>
        </ModalContent>
      </Modal>

      <Modal
        isOpen={updateResult !== null}
        onClose={() => setUpdateResult(null)}
        isCentered
      >
        <ModalOverlay />
        <ModalContent>
          <ModalHeader>共享实例同步成功</ModalHeader>
          <ModalBody>
            <VStack align="stretch" spacing={2}>
              <Text>已保留 {updateResult?.skipped.length || 0} 个现有文件</Text>
              <Text>已移除 {updateResult?.deleted.length || 0} 个旧文件</Text>
              <Text>
                已新增 {updateResult?.downloaded.length || 0} 个新文件
              </Text>
              <Text>
                已更新 {updateResult?.updated.length || 0} 个不一致文件
              </Text>
            </VStack>
          </ModalBody>
          <ModalFooter>
            <Button colorScheme="blue" onClick={() => setUpdateResult(null)}>
              完成
            </Button>
          </ModalFooter>
        </ModalContent>
      </Modal>

      <Modal
        isOpen={bindingDialogMode !== null}
        onClose={() => !isUpdating && setBindingDialogMode(null)}
        isCentered
      >
        <ModalOverlay />
        <ModalContent>
          <ModalHeader>
            {bindingDialogMode === "select" ? "绑定本地实例" : "确认更新实例"}
          </ModalHeader>
          <ModalBody>
            {bindingDialogMode === "select" ? (
              localInstances.length ? (
                <RadioGroup
                  value={chosenInstanceId}
                  onChange={setChosenInstanceId}
                >
                  <VStack align="stretch" spacing={2}>
                    {localInstances.map((instance: InstanceSummary) => (
                      <Box
                        key={instance.id}
                        borderWidth="1px"
                        borderRadius="md"
                        px={3}
                        py={2}
                        cursor="pointer"
                        onClick={() => setChosenInstanceId(instance.id)}
                      >
                        <HStack>
                          <Radio value={instance.id} />
                          <Box>
                            <Text>{instance.name}</Text>
                            <Text fontSize="xs" className="secondary-text">
                              {instance.version}
                            </Text>
                          </Box>
                        </HStack>
                      </Box>
                    ))}
                  </VStack>
                </RadioGroup>
              ) : (
                <Text>还没有本地实例，请先创建或导入一个实例。</Text>
              )
            ) : (
              <Text>
                将使用本地实例“{selectedLocalInstance?.name || boundInstanceId}
                ”更新。共享实例中标记删除的文件会移除；使用中的文件在本地缺失或
                SHA-256
                不一致时会重新下载。未列入共享实例的本地文件不会受到影响。
              </Text>
            )}
          </ModalBody>
          <ModalFooter>
            <HStack spacing={3}>
              <Button
                variant="ghost"
                onClick={() => setBindingDialogMode(null)}
              >
                取消
              </Button>
              {bindingDialogMode === "confirm" && (
                <Button onClick={() => setBindingDialogMode("select")}>
                  更换
                </Button>
              )}
              <Button
                colorScheme="blue"
                isLoading={isUpdating}
                isDisabled={
                  (bindingDialogMode === "select" && !chosenInstanceId) ||
                  localInstances.length === 0
                }
                onClick={() =>
                  bindingDialogMode === "select"
                    ? bindAndUpdate()
                    : runUpdate(boundInstanceId)
                }
              >
                确认
              </Button>
            </HStack>
          </ModalFooter>
        </ModalContent>
      </Modal>
    </>
  );
};

export default SharedInstancesPage;
