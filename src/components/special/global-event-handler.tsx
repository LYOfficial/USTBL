import { useRouter } from "next/router";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useMemo, useRef } from "react";
import { useLauncherConfig } from "@/contexts/config";
import { useGlobalData } from "@/contexts/global-data";
import { useSharedModals } from "@/contexts/shared-modal";
import useDeepLink from "@/hooks/deep-link";
import { useDragAndDrop, useTauriFileDrop } from "@/hooks/drag-and-drop";
import useKeyboardShortcut from "@/hooks/keyboard-shortcut";
import { useToast } from "@/contexts/toast";
import { AccountService } from "@/services/account";

// Handle global keyboard shortcuts, DnD events, etc.
const GlobalEventHandler: React.FC<{ children: React.ReactNode }> = ({
  children,
}) => {
  const { openSharedModal } = useSharedModals();
  const toast = useToast();
  const { selectedInstance } = useGlobalData();
  const { newerVersion } = useLauncherConfig();
  const router = useRouter();
  const isStandAlone =
    router.pathname.startsWith("/standalone") || router.pathname === "/tray-popup";
  const hasNotifiedNewVersion = useRef(false);
  const knownMessageIds = useRef<Set<number> | null>(null);

  useEffect(() => {
    let disposed = false;
    const setup = async () => {
      const [launchUnlisten, launchRequestUnlisten, launchErrorUnlisten, friendsUnlisten, accelerationUnlisten, accelerationErrorUnlisten] = await Promise.all([
        listen<string>("ustbl:tray-launch", (event) => {
          if (!disposed && event.payload) openSharedModal("launch", { instanceId: event.payload });
        }),
        listen("ustbl:tray-launch-request", () => {
          if (selectedInstance && !disposed) {
            openSharedModal("launch", { instanceId: selectedInstance.id });
          }
        }),
        listen<string>("ustbl:tray-launch-failed", (event) => {
          if (!disposed) toast({ title: "启动游戏失败", description: event.payload, status: "error" });
        }),
        listen("ustbl:tray-friends", () => {
          if (!disposed) openSharedModal("vustb-friends", { isTray: true });
        }),
        listen("ustbl:tray-acceleration-started", () => {
          if (!disposed) toast({ title: "GitHub 加速已启动", status: "success" });
        }),
        listen<string>("ustbl:tray-acceleration-failed", (event) => {
          if (!disposed) toast({ title: "GitHub 加速启动失败", description: event.payload, status: "error" });
        }),
      ]);
      return () => {
        launchUnlisten();
        launchRequestUnlisten();
        launchErrorUnlisten();
        friendsUnlisten();
        accelerationUnlisten();
        accelerationErrorUnlisten();
      };
    };
    const cleanup = setup();
    return () => {
      disposed = true;
      void cleanup.then((value) => value?.());
    };
  }, [openSharedModal, selectedInstance, toast]);

  useEffect(() => {
    if (
      !isStandAlone &&
      newerVersion.version &&
      !hasNotifiedNewVersion.current
    ) {
      hasNotifiedNewVersion.current = true;
      openSharedModal("notify-new-version", { newVersion: newerVersion });
    }
  }, [isStandAlone, newerVersion, openSharedModal]);

  useEffect(() => {
    if (isStandAlone || router.pathname.startsWith("/messages")) return;
    let disposed = false;
    let streamStarted = false;
    let nextAccountRetryAt = 0;
    let accountRetryDelay = 30000;
    let nextServerMessageRetryAt = 0;
    const poll = async () => {
      const now = Date.now();
      if (now < nextAccountRetryAt) return;
      const account = await AccountService.retrieveVustbAccount();
      if (disposed || account.status !== "success" || !account.data) {
        streamStarted = false;
        knownMessageIds.current = null;
        nextAccountRetryAt = Date.now() + accountRetryDelay;
        accountRetryDelay = Math.min(accountRetryDelay * 2, 300000);
        return;
      }
      nextAccountRetryAt = 0;
      accountRetryDelay = 30000;
      if (!streamStarted) {
        streamStarted = true;
        void AccountService.startVustbFriendMessageStream();
      }
      if (router.pathname.startsWith("/messages") || Date.now() < nextServerMessageRetryAt) return;
      const response = await AccountService.retrieveVustbServerMessages();
      if (disposed || response.status !== "success") {
        nextServerMessageRetryAt = Date.now() + 30000;
        return;
      }
      nextServerMessageRetryAt = 0;
      const messages = response.data.flatMap((group) => group.messages);
      const ids = new Set(messages.map((message) => message.id));
      if (knownMessageIds.current === null) {
        knownMessageIds.current = ids;
        return;
      }
      const quiet = window.localStorage.getItem("ustbl.message.quiet.global") === "true";
      if (!quiet) {
        const hidden = typeof window !== "undefined" && !(await getCurrentWindow().isVisible().catch(() => true));
        if (hidden) {
          const freshMessages = messages
            .filter((message) => !knownMessageIds.current?.has(message.id))
            .filter((message) => window.localStorage.getItem(`ustbl.message.quiet.server.${message.serverId}`) !== "true")
            .slice(-5);
          for (const message of freshMessages) {
            void invoke("show_message_notification", { message: `${message.serverName} · ${message.sender}: ${message.content}` });
          }
        }
      }
      knownMessageIds.current = ids;

    };
    void poll();
    const unlisten = AccountService.onVustbFriendMessage(async (message) => {
      if (disposed || window.localStorage.getItem("ustbl.message.quiet.global") === "true") return;
      const account = await AccountService.retrieveVustbAccount();
      if (account.status !== "success" || !account.data) return;
      const currentUserId = Number(account.data.subject);
      const friendId = message.sender_id === currentUserId ? message.recipient_id : message.sender_id;
      if (window.localStorage.getItem(`ustbl.message.quiet.friend.${friendId}`) === "true") return;
      const hidden = !(await getCurrentWindow().isVisible().catch(() => true));
      if (hidden) void invoke("show_message_notification", { message: `${message.sender}: ${message.content}` });
    });
    const timer = window.setInterval(() => void poll(), 30000);
    return () => { disposed = true; window.clearInterval(timer); unlisten?.(); };
  }, [isStandAlone, router.pathname]);

  // ----------------- Keyboard Shortcuts -----------------
  const spotlightShortcuts = useMemo(
    () => ({
      macos: { metaKey: true, key: "S" },
      windows: { ctrlKey: true, key: "S" },
      linux: { ctrlKey: true, key: "S" },
    }),
    []
  );

  const openSpotlightSearch = useCallback(() => {
    if (!isStandAlone) openSharedModal("spotlight-search");
  }, [isStandAlone, openSharedModal]);

  useKeyboardShortcut(spotlightShortcuts, openSpotlightSearch);

  // ------------------- Drag and Drops -------------------

  const addAuthServerByDnD = useCallback(
    (data: string) => {
      const prefix = "authlib-injector:yggdrasil-server:";
      if (data.startsWith(prefix)) {
        const url = data.slice(prefix.length);
        const decodeUrl = decodeURIComponent(url);
        if (!isStandAlone && decodeUrl)
          openSharedModal("add-auth-server", { presetUrl: decodeUrl });
      }
    },
    [isStandAlone, openSharedModal]
  );

  useDragAndDrop({
    onDrop: addAuthServerByDnD,
  });

  const importModpackByDnD = useCallback(
    (path: string) => {
      if (!isStandAlone) openSharedModal("import-modpack", { path });
    },
    [isStandAlone, openSharedModal]
  );

  useTauriFileDrop({
    pattern: "\\.(zip|mrpack)$",
    onMatch: importModpackByDnD,
  });

  // ---------------------- Deeplinks ---------------------

  // Note: These triggers appear to be ordinary strings on the surface,
  //       but they are actually syntactic sugar for JavaScript,
  //       being parsed into RegExp objects,
  //       which can affect the `Object.is()` comparison.
  const addAuthServerTrigger = useMemo(
    () => /^add-auth-server\/?(?:\?.*)?$/,
    []
  );
  const launchTrigger = useMemo(() => /^launch\/?(?:\?.*)?$/, []);

  const addAuthServerByDeeplink = useCallback(
    (path: string | URL) => {
      const url = new URL(path).searchParams.get("url") || "";
      const decodeUrl = decodeURIComponent(url);
      if (!isStandAlone && decodeUrl) {
        openSharedModal("add-auth-server", { presetUrl: decodeUrl });
      }
    },
    [isStandAlone, openSharedModal]
  );

  const quickLaunchGame = useCallback(
    (path: string | URL) => {
      const id = new URL(path).searchParams.get("id") || "";
      const decodeId = decodeURIComponent(id);
      if (!isStandAlone && decodeId) {
        // Delay the modal opening to ensure required app state/data (e.g. selected player in global-data context) is ready.
        // This is important when the app is opened via deeplink.
        // FIXME: find a better way to handle this.
        setTimeout(() => {
          openSharedModal("launch", { instanceId: decodeId });
        }, 500);
      }
    },
    [isStandAlone, openSharedModal]
  );

  useDeepLink({
    trigger: addAuthServerTrigger,
    onCall: addAuthServerByDeeplink,
  });

  useDeepLink({
    trigger: launchTrigger,
    onCall: quickLaunchGame,
  });

  return <>{children}</>;
};

export default GlobalEventHandler;
