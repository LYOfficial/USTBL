import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { useLauncherConfig } from "@/contexts/config";
import { useToast } from "@/contexts/toast";
import {
  GetStateFlag,
  useGetState,
  usePromisedGetState,
} from "@/hooks/get-state";
import { AuthServer, Player } from "@/models/account";
import { InstanceSummary } from "@/models/instance/misc";
import { ModpackUpdateCheck } from "@/models/modpack-update";
import { GameClientResourceInfo } from "@/models/resource";
import { AccountService } from "@/services/account";
import { CampusModpackService } from "@/services/campus-modpack";
import { InstanceService } from "@/services/instance";
import { ModpackUpdateService } from "@/services/modpack-update";
import { ResourceService } from "@/services/resource";
import {
  installingCheck,
  isInstallingInstance,
  shouldCheckInstance,
  shouldPrimeCampusListing,
} from "@/utils/installing-instance";

interface GlobalDataContextType {
  selectedPlayer: Player | undefined;
  selectedInstance: InstanceSummary | undefined;
  getPlayerList: (sync?: boolean) => Player[] | undefined;
  getInstanceList: (sync?: boolean) => InstanceSummary[] | undefined;
  getAuthServerList: (sync?: boolean) => AuthServer[] | undefined;
  getGameVersionList: (
    sync?: boolean
  ) => Promise<GameClientResourceInfo[] | GetStateFlag | undefined>;
  isGameVersionListLoading: boolean;
  /** Latest modpack update check per followed instance, by instance id. */
  modpackChecks: Record<string, ModpackUpdateCheck>;
  /** Check one instance and remember the result. */
  checkModpackUpdate: (
    instanceId: string,
    scanLocal?: boolean
  ) => Promise<ModpackUpdateCheck>;
  /**
   * True once the startup check of every followed instance has finished.
   * The modpack update reminder listens to this so it fires exactly once.
   */
  startupChecksDone: boolean;
}

// for frontend-only state update
interface GlobalDataDispatchContextType {
  setPlayerList: React.Dispatch<Player[]>;
  setSelectedPlayer: React.Dispatch<Player | undefined>;
  setInstanceList: React.Dispatch<InstanceSummary[]>;
  setSelectedInstance: React.Dispatch<InstanceSummary | undefined>;
  setAuthServerList: React.Dispatch<AuthServer[]>;
  setGameVersionList: React.Dispatch<GameClientResourceInfo[]>;
}

const GlobalDataContext = createContext<GlobalDataContextType | undefined>(
  undefined
);

const GlobalDataDispatchContext = createContext<
  GlobalDataDispatchContextType | undefined
>(undefined);

export const GlobalDataContextProvider: React.FC<{
  children: React.ReactNode;
}> = ({ children }) => {
  const { t } = useTranslation();
  const { config } = useLauncherConfig();
  const toast = useToast();

  const [playerList, setPlayerList] = useState<Player[]>();
  const [selectedPlayer, setSelectedPlayer] = useState<Player>();
  const [instanceList, setInstanceList] = useState<InstanceSummary[]>();
  const [selectedInstance, setSelectedInstance] = useState<InstanceSummary>();
  const [authServerList, setAuthServerList] = useState<AuthServer[]>();
  const [gameVersionList, setGameVersionList] =
    useState<GameClientResourceInfo[]>();

  const [modpackChecks, setModpackChecks] = useState<
    Record<string, ModpackUpdateCheck>
  >({});
  const [startupChecksDone, setStartupChecksDone] = useState(false);
  const startupChecked = useRef(false);
  // An auto-unlink is announced once per instance, not on every later check.
  const unlinkNotified = useRef(new Set<string>());
  // The unlink toast names the instance, but checkModpackUpdate must stay
  // referentially stable across instance list updates.
  const instanceListRef = useRef(instanceList);
  instanceListRef.current = instanceList;

  const handleRetrievePlayerList = useCallback(() => {
    AccountService.retrievePlayerList().then((response) => {
      if (response.status === "success") {
        setPlayerList(response.data);
      } else {
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
      }
    });
  }, [toast]);

  const handleRetrieveAuthServerList = useCallback(() => {
    AccountService.retrieveAuthServerList().then((response) => {
      if (response.status === "success") setAuthServerList(response.data);
      else
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
    });
  }, [setAuthServerList, toast]);

  const handleRetrieveInstanceList = useCallback(() => {
    InstanceService.retrieveInstanceList().then((response) => {
      if (response.status === "success") {
        const sorted = [...response.data].sort((a, b) => {
          // put starred instances at the top
          return Number(b.starred) - Number(a.starred);
        });
        setInstanceList(sorted);
        setModpackChecks((current) =>
          Object.fromEntries(
            Object.entries(current).filter(
              ([id, check]) =>
                check.status !== "installing" ||
                isInstallingInstance(
                  sorted.find((instance) => instance.id === id)
                )
            )
          )
        );
        return response.data;
      } else
        toast({
          title: response.message,
          description: response.details,
          status: "error",
        });
    });
  }, [setInstanceList, toast]);

  const checkModpackUpdate = useCallback(
    async (instanceId: string, scanLocal = false, afterInstall = false) => {
      const instance = instanceListRef.current?.find(
        (item) => item.id === instanceId
      );
      const response =
        !afterInstall && isInstallingInstance(instance)
          ? { status: "success" as const, data: installingCheck(instance!) }
          : await ModpackUpdateService.checkModpackUpdate(
              instanceId,
              scanLocal
            );
      const check: ModpackUpdateCheck =
        response.status === "success"
          ? response.data
          : {
              status: "unknown",
              follow: true,
              reason: response.details,
              canRollBack: false,
            };
      setModpackChecks((current) => ({ ...current, [instanceId]: check }));
      if (check.unlinked && !unlinkNotified.current.has(instanceId)) {
        unlinkNotified.current.add(instanceId);
        const name = instanceListRef.current?.find(
          (i) => i.id === instanceId
        )?.name;
        toast({
          title: t("InstanceSyncPage.ledger.unlinkedToast", {
            name: name ?? instanceId,
          }),
          status: "info",
        });
        // The platform confirmed the project is gone; refresh the instance
        // list so the ledger row drops the origin right away.
        handleRetrieveInstanceList();
      }
      return check;
    },
    [toast, t, handleRetrieveInstanceList]
  );

  // Once per launch, look for updates of every followed instance. One at a
  // time, so a slow platform does not pile up requests. When all are done the
  // reminder may fire.
  useEffect(() => {
    if (!instanceList || startupChecked.current) return;
    startupChecked.current = true;
    (async () => {
      // Campus instances share one project listing instead of a request each.
      const primed = shouldPrimeCampusListing(instanceList);
      if (primed) await ModpackUpdateService.primeCampusListing();
      try {
        for (const instance of instanceList) {
          if (shouldCheckInstance(instance))
            await checkModpackUpdate(instance.id);
        }
      } finally {
        if (primed) await ModpackUpdateService.clearCampusListing();
      }
      setStartupChecksDone(true);
    })();
  }, [instanceList, checkModpackUpdate]);

  useEffect(
    () =>
      CampusModpackService.onInstallFinished((event) => {
        if (event.error) return;
        // The disk record has gone; bypass the old summary until its refresh lands.
        setInstanceList((current) =>
          current?.map((instance) =>
            instance.id === event.instanceId
              ? { ...instance, campusInstallPending: false }
              : instance
          )
        );
        void checkModpackUpdate(event.instanceId, false, true);
        handleRetrieveInstanceList();
      }),
    [checkModpackUpdate, handleRetrieveInstanceList]
  );

  useEffect(() => {
    return InstanceService.onPlayTimeUpdated(({ instanceId, playTime }) => {
      setInstanceList((current) =>
        current?.map((instance) =>
          instance.id === instanceId ? { ...instance, playTime } : instance
        )
      );
    });
  }, []);

  useEffect(() => {
    const selectedPlayerId = config.states.shared.selectedPlayerId;
    setSelectedPlayer(
      playerList?.find((player) => player.id === selectedPlayerId)
    );
  }, [playerList, config.states.shared.selectedPlayerId]);

  useEffect(() => {
    const selectedInstanceId = config.states.shared.selectedInstanceId;
    setSelectedInstance(
      instanceList?.find((instance) => instance.id === selectedInstanceId)
    );
  }, [instanceList, config.states.shared.selectedInstanceId]);

  const handleFetchGameVersionList = useCallback(async () => {
    const response = await ResourceService.fetchGameVersionList();
    if (response.status === "success") {
      setGameVersionList(response.data);
      return response.data;
    } else {
      toast({
        title: response.message,
        description: response.details,
        status: "error",
      });
      setGameVersionList([]);
      return [];
    }
  }, [setGameVersionList, toast]);

  const getPlayerList = useGetState(playerList, handleRetrievePlayerList);

  // Note: Do not apply any post-processing process on the local state,
  //       as it will be passed into `useState` dependencies.
  const getInstanceList = useGetState(instanceList, handleRetrieveInstanceList);

  const getAuthServerList = useGetState(
    authServerList,
    handleRetrieveAuthServerList
  );

  const [getGameVersionList, isGameVersionListLoading] = usePromisedGetState(
    gameVersionList,
    React.useRef("global"), // version is static as the data is not instance-specific
    handleFetchGameVersionList
  );

  return (
    <GlobalDataContext.Provider
      value={{
        selectedPlayer,
        selectedInstance,
        getPlayerList,
        getInstanceList,
        getAuthServerList,
        getGameVersionList,
        isGameVersionListLoading,
        modpackChecks,
        checkModpackUpdate,
        startupChecksDone,
      }}
    >
      <GlobalDataDispatchContext.Provider
        value={{
          setPlayerList,
          setSelectedPlayer,
          setInstanceList,
          setSelectedInstance,
          setAuthServerList,
          setGameVersionList,
        }}
      >
        {children}
      </GlobalDataDispatchContext.Provider>
    </GlobalDataContext.Provider>
  );
};

export const useGlobalData = (): GlobalDataContextType => {
  const context = useContext(GlobalDataContext);
  if (!context) {
    throw new Error(
      "useGlobalData must be used within a GlobalDataContextProvider"
    );
  }
  return context;
};

export const useGlobalDataDispatch = (): GlobalDataDispatchContextType => {
  const context = useContext(GlobalDataDispatchContext);
  if (!context) {
    throw new Error(
      "useGlobalDataDispatch must be used within a GlobalDataContextProvider"
    );
  }
  return context;
};
