import DownloadSpecificResourceModal from "@/components//modals/download-specific-resource-modal";
import AddAuthServerModal from "@/components/modals/add-auth-server-modal";
import AlertResourceDependencyModal from "@/components/modals/alert-resource-dependency-modal";
import CopyOrMoveModal from "@/components/modals/copy-or-move-modal";
import DeleteInstanceDialog from "@/components/modals/delete-instance-alert-dialog";
import DownloadResourceModal from "@/components/modals/download-resource-modal";
import ExportModpackModal from "@/components/modals/export-modpack-modal";
import GenericConfirmDialog from "@/components/modals/generic-confirm-dialog";
import ImportModpackModal from "@/components/modals/import-modpack-modal";
import InstallPlatformModpackModal from "@/components/modals/install-platform-modpack-modal";
import LaunchProcessModal from "@/components/modals/launch-process-modal";
import ModpackInstanceDetailsModal from "@/components/modals/modpack-instance-details-modal";
import ModpackLocalChangesModal from "@/components/modals/modpack-local-changes-modal";
import NotifyModpackUpdatesModal from "@/components/modals/notify-modpack-updates-modal";
import NotifyNewVersionModal from "@/components/modals/notify-new-version-modal";
import PublishModpackModal from "@/components/modals/publish-modpack-modal";
import ReLoginPlayerModal from "@/components/modals/relogin-player-modal";
import SpotlightSearchModal from "@/components/modals/spotlight-search-modal";
import UpdateModpackModal from "@/components/modals/update-modpack-modal";
import VustbAnnouncementModal from "@/components/modals/vustb-announcement-modal";
import VustbFriendsModal from "@/components/modals/vustb-friends-modal";
import VustbLoginRequiredModal from "@/components/modals/vustb-login-required-modal";
import { SharedModalContextProvider } from "@/contexts/shared-modal";
import { useSharedModals } from "@/contexts/shared-modal";

const SharedModalsProvider: React.FC<{ children: React.ReactNode }> = ({
  children,
}) => {
  return (
    <SharedModalContextProvider>
      <SharedModals>{children}</SharedModals>
    </SharedModalContextProvider>
  );
};

const SharedModals: React.FC<{ children: React.ReactNode }> = ({
  children,
}) => {
  const { modalStates, closeSharedModal } = useSharedModals();

  const modals: Record<string, React.FC<any>> = {
    "add-auth-server": AddAuthServerModal,
    "alert-resource-dependency": AlertResourceDependencyModal,
    "copy-or-move": CopyOrMoveModal,
    "delete-instance-alert": DeleteInstanceDialog,
    "download-resource": DownloadResourceModal,
    "download-specific-resource": DownloadSpecificResourceModal,
    "export-modpack": ExportModpackModal,
    "generic-confirm": GenericConfirmDialog,
    "import-modpack": ImportModpackModal,
    "install-platform-modpack": InstallPlatformModpackModal,
    launch: LaunchProcessModal,
    "modpack-instance-details": ModpackInstanceDetailsModal,
    "modpack-local-changes": ModpackLocalChangesModal,
    "notify-modpack-updates": NotifyModpackUpdatesModal,
    "notify-new-version": NotifyNewVersionModal,
    "publish-modpack": PublishModpackModal,
    relogin: ReLoginPlayerModal,
    "spotlight-search": SpotlightSearchModal,
    "update-modpack": UpdateModpackModal,
    "vustb-announcement": VustbAnnouncementModal,
    "vustb-friends": VustbFriendsModal,
    "vustb-login-required": VustbLoginRequiredModal,
  };

  return (
    <>
      {children}

      {Object.keys(modals).map((key) => {
        const modalParams = modalStates[key];
        if (!modalParams) return null;

        const SpecModal = modals[key];
        return (
          <SpecModal
            key={key}
            {...modalParams}
            onClose={() => closeSharedModal(key)}
          />
        );
      })}
    </>
  );
};

export default SharedModalsProvider;
