import React, { createContext, useContext, useState } from "react";
import { useLauncherConfig } from "@/contexts/config";
import { AccountService } from "@/services/account";

const ACCOUNT_REQUIRED_MODAL_KEYS = new Set([
  "launch",
  "download-resource",
  "download-specific-resource",
  "import-modpack",
  "update-modpack",
  "delete-instance-alert",
  "export-modpack",
  "publish-modpack",
  "modpack-local-changes",
  "copy-or-move",
]);

interface SharedModalContextType {
  openSharedModal: (key: string, params?: any) => void;
  requireVustbAccount: (onAllowed: () => void) => void;
  closeSharedModal: (key: string) => void;
  openGenericConfirmDialog: (params?: any) => void;
  modalStates: Record<string, { isOpen: boolean; params: any }>;
}

export const SharedModalContext = createContext<
  SharedModalContextType | undefined
>(undefined);

export const SharedModalContextProvider: React.FC<{
  children: React.ReactNode;
}> = ({ children }) => {
  const [modalStates, setModalStates] = useState<
    Record<string, { isOpen: boolean; params: any }>
  >({});
  const { config } = useLauncherConfig();

  const openModalState = (key: string, params: any = {}) => {
    setModalStates((prev) => ({
      ...prev,
      [key]: { isOpen: true, ...params },
    }));
    logger.info("Opened shared modal:", key);
  };

  const requireVustbAccount = (onAllowed: () => void) => {
    void AccountService.retrieveVustbAccount()
      .then((response) => {
        if (response.status === "success" && response.data) {
          onAllowed();
        } else {
          openModalState("vustb-login-required", {});
        }
      })
      .catch(() => openModalState("vustb-login-required", {}));
  };

  const openSharedModal = (key: string, params: any = {}) => {
    if (ACCOUNT_REQUIRED_MODAL_KEYS.has(key)) {
      requireVustbAccount(() => openModalState(key, params));
      return;
    }
    openModalState(key, params);
  };

  const closeSharedModal = (key: string) => {
    setModalStates((prev) => {
      const { [key]: _, ...newStates } = prev;
      return newStates;
    });
  };

  const openGenericConfirmDialog = (params?: any) => {
    // If the user has previously selected "Don't show again", skip the dialog and call the OK callback directly
    if (
      params.suppressKey &&
      config.suppressedDialogs?.includes(params.suppressKey)
    ) {
      params?.onOKCallback?.();
      return;
    }
    openSharedModal("generic-confirm", {
      ...params,
    });
  };

  return (
    <SharedModalContext.Provider
      value={{
        openSharedModal,
        requireVustbAccount,
        closeSharedModal,
        openGenericConfirmDialog,
        modalStates,
      }}
    >
      {children}
    </SharedModalContext.Provider>
  );
};

export const useSharedModals = (): SharedModalContextType => {
  const context = useContext(SharedModalContext);
  if (!context) {
    throw new Error(
      "useSharedModals must be used within a SharedModalContextProvider"
    );
  }
  return context;
};
