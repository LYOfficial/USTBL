import { Icon, VStack, useDisclosure } from "@chakra-ui/react";
import { open } from "@tauri-apps/plugin-dialog";
import { useRouter } from "next/router";
import { useTranslation } from "react-i18next";
import { LuArrowRight } from "react-icons/lu";
import {
  OptionItemGroup,
  OptionItemGroupProps,
} from "@/components/common/option-item";
import { CreateInstanceModal } from "@/components/modals/create-instance-modal";
import { useSharedModals } from "@/contexts/shared-modal";

const AddAndImportInstancePage = () => {
  const { t } = useTranslation();
  const router = useRouter();
  const { openSharedModal, requireVustbAccount } = useSharedModals();

  const {
    isOpen: isCreateInstanceModalOpen,
    onOpen: onOpenCreateInstanceModal,
    onClose: onCloseCreateInstanceModal,
  } = useDisclosure();
  const handleImportModpackFromDisk = async () => {
    requireVustbAccount(async () => {
      const filePath = await open({
        multiple: false,
        filters: [
          {
            name: t("General.dialog.filterName.modpack"),
            extensions: ["zip", "mrpack"],
          },
        ],
      });
      if (filePath) openSharedModal("import-modpack", { path: filePath });
    });
  };

  const addAndImportOptions: Record<string, () => void> = {
    new: () => requireVustbAccount(onOpenCreateInstanceModal),
    modpack: handleImportModpackFromDisk,
    campusModpack: () => router.push("/instances/sync"),
    manageDirs: () => router.push("/settings/global-game"),
  };

  const optionGroups: OptionItemGroupProps[] = [
    {
      title: t("AllInstancesPage.button.addAndImport"),
      items: Object.keys(addAndImportOptions).map((key) => ({
        title: t(`AddAndImportInstancePage.addAndImportOptions.${key}.title`),
        description: t(
          `AddAndImportInstancePage.addAndImportOptions.${key}.description`
        ),
        children: <Icon as={LuArrowRight} boxSize={3.5} mr="5px" />,
        isFullClickZone: true,
        onClick: addAndImportOptions[key],
      })),
    },
  ];

  return (
    <>
      <VStack w="100%" spacing={4}>
        {optionGroups.map((group, index) => (
          <OptionItemGroup w="100%" {...group} key={index} />
        ))}
      </VStack>
      <CreateInstanceModal
        isOpen={isCreateInstanceModalOpen}
        onClose={onCloseCreateInstanceModal}
      />
    </>
  );
};

export default AddAndImportInstancePage;
