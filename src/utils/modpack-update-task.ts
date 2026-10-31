import type { TFunction } from "i18next";
import type { ModpackUpdateFinished } from "@/models/modpack-update";
import {
  failedUpdatePlans,
  rememberedChoices,
} from "@/utils/modpack-update-choices";

export const isModpackUpdateGroup = (group: string): boolean =>
  group.startsWith("modpack-update?");

export const localizeModpackUpdateError = (t: TFunction, reason?: string) => {
  const category =
    reason &&
    /network|connection|timeout|timed out|request|http|status|range ended/i.test(
      reason
    )
      ? "NETWORK_ERROR"
      : reason &&
          /os error|permission|access.*denied|disk|space|file creation|read.only/i.test(
            reason
          )
        ? "FILE_OPERATION_ERROR"
        : reason || "UNKNOWN_ERROR";
  return t(
    `Services.instance.applyModpackUpdate.error.description.${category}`,
    {
      defaultValue: t(`UpdateModpackModal.reason.${category}`, {
        defaultValue: t(
          "Services.instance.applyModpackUpdate.error.description.UNKNOWN_ERROR"
        ),
      }),
    }
  );
};

/** The global task context owns completion, never the disposable dialog. */
export function finishModpackUpdate(
  payload: ModpackUpdateFinished,
  handlers: {
    refresh: () => void;
    check: (instanceId: string, scanLocal: boolean) => unknown;
    notify: (failed: boolean, error?: string) => void;
    loaderPending: () => void;
  }
) {
  handlers.refresh();
  if (payload.error) {
    failedUpdatePlans.set(payload.instanceId, payload.planHash);
    handlers.notify(true, payload.error);
    return;
  }
  failedUpdatePlans.delete(payload.instanceId);
  rememberedChoices.delete(payload.instanceId);
  void handlers.check(payload.instanceId, true);
  handlers.notify(false);
  if (payload.outcome?.loaderInstallPending) handlers.loaderPending();
}
