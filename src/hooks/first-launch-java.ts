import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useLauncherConfig } from "@/contexts/config";
import { useToast } from "@/contexts/toast";
import { ConfigService } from "@/services/config";

/**
 * On the very first launch (`runCount === 0`), make sure Java 25 is available.
 * If it is not installed yet, download it through the launcher's own Mojang
 * Java installer (`downloadMojangJava`). A localStorage flag keeps a first
 * launch that is interrupted before the terms dialog from starting duplicate
 * downloads on every retry.
 */
const JAVA25_VERSION = 25;
const STORAGE_KEY = "ustbl.firstLaunch.java25.installed";

export const useFirstLaunchJava25 = (enabled = true) => {
  const { config } = useLauncherConfig();
  const toast = useToast();
  const { t } = useTranslation();

  useEffect(() => {
    if (!enabled || config.mocked || config.runCount !== 0) return;
    if (window.localStorage.getItem(STORAGE_KEY) === "done") return;

    ConfigService.retrieveJavaList()
      .then((response) => {
        if (response.status !== "success") return;

        const hasJava25 = response.data.some(
          (java) => java.majorVersion === JAVA25_VERSION
        );
        if (hasJava25) {
          window.localStorage.setItem(STORAGE_KEY, "done");
          return;
        }

        return ConfigService.downloadMojangJava(String(JAVA25_VERSION)).then(
          (download) => {
            if (download.status === "success") {
              window.localStorage.setItem(STORAGE_KEY, "done");
              toast({
                title: t("FirstLaunchJava25.autoInstall.title"),
                description: t("FirstLaunchJava25.autoInstall.description"),
                status: "success",
              });
            } else {
              toast({
                title: download.message,
                description: download.details,
                status: "error",
              });
            }
          }
        );
      })
      .catch(() => {
        // Best effort only: never block the first launch on a Java check.
      });
  }, [enabled, config.mocked, config.runCount, t, toast]);
};
