import { invoke } from "@tauri-apps/api/core";
import {
  ModpackConflictChoice,
  ModpackLocalChange,
  ModpackUpdateCheck,
  ModpackUpdateOutcome,
  ModpackUpdatePlan,
} from "@/models/modpack-update";
import { InvokeResponse } from "@/models/response";
import { responseHandler } from "@/utils/response";

export class ModpackUpdateService {
  /**
   * Ask the platform whether the followed modpack has a newer version. Cheap:
   * nothing is downloaded. `scanLocal` also hashes the managed files to tell
   * whether the player changed any (slower, so only on demand).
   */
  @responseHandler("instance")
  static async checkModpackUpdate(
    instanceId: string,
    scanLocal = false
  ): Promise<InvokeResponse<ModpackUpdateCheck>> {
    return await invoke("check_modpack_update", { instanceId, scanLocal });
  }

  /**
   * Put the instance back the way it was before the last update. Resolves to
   * whether there was a backup to restore.
   */
  @responseHandler("instance")
  static async rollbackModpackUpdate(
    instanceId: string
  ): Promise<InvokeResponse<boolean>> {
    return await invoke("rollback_modpack_update", { instanceId });
  }

  /**
   * Compare the new version with the instance. Changes no instance file.
   */
  @responseHandler("instance")
  static async prepareModpackUpdate(
    instanceId: string
  ): Promise<InvokeResponse<ModpackUpdatePlan>> {
    return await invoke("prepare_modpack_update", { instanceId });
  }

  /**
   * Stop the running preview of this instance, if any. The cancelled preview
   * answers with `MODPACK_UPDATE_PREPARE_CANCELLED`, which callers ignore.
   */
  @responseHandler("instance")
  static async cancelModpackUpdatePrepare(
    instanceId: string
  ): Promise<InvokeResponse<null>> {
    return await invoke("cancel_modpack_update_prepare", { instanceId });
  }

  /**
   * Apply the plan the player confirmed. Conflicts without an answer keep the
   * local file.
   */
  @responseHandler("instance")
  static async applyModpackUpdate(
    instanceId: string,
    planHash: string,
    choices: Record<string, ModpackConflictChoice>
  ): Promise<InvokeResponse<ModpackUpdateOutcome>> {
    return await invoke("apply_modpack_update", {
      instanceId,
      planHash,
      choices,
    });
  }

  /**
   * The managed files changed since the modpack installed them, with line
   * diffs for text files. Local only; the platform is not asked.
   */
  @responseHandler("instance")
  static async listModpackLocalChanges(
    instanceId: string
  ): Promise<InvokeResponse<ModpackLocalChange[]>> {
    return await invoke("list_modpack_local_changes", { instanceId });
  }

  @responseHandler("instance")
  static async revealModpackConflictFile(
    instanceId: string,
    relativePath: string
  ): Promise<InvokeResponse<null>> {
    return await invoke("reveal_modpack_conflict_file", {
      instanceId,
      relativePath,
    });
  }

  /**
   * Undo or finish an interrupted update. Resolves to whether anything was done.
   */
  @responseHandler("instance")
  static async recoverModpackUpdate(
    instanceId: string
  ): Promise<InvokeResponse<boolean>> {
    return await invoke("recover_modpack_update", { instanceId });
  }

  @responseHandler("instance")
  static async setModpackFollow(
    instanceId: string,
    follow: boolean
  ): Promise<InvokeResponse<null>> {
    return await invoke("set_modpack_follow", { instanceId, follow });
  }

  /**
   * Detach the instance from its modpack: identity and baseline go, the game
   * files stay. Refused while an update transaction is unfinished.
   */
  @responseHandler("instance")
  static async unlinkModpackOrigin(
    instanceId: string
  ): Promise<InvokeResponse<null>> {
    return await invoke("unlink_modpack_origin", { instanceId });
  }

  /** The modpack version the launcher already announced for this instance. */
  @responseHandler("instance")
  static async getModpackUpdateNotice(
    instanceId: string
  ): Promise<InvokeResponse<string | null>> {
    return await invoke("get_modpack_update_notice", { instanceId });
  }

  /** Remember that the launcher announced `versionId` for this instance. */
  @responseHandler("instance")
  static async setModpackUpdateNotice(
    instanceId: string,
    versionId: string
  ): Promise<InvokeResponse<null>> {
    return await invoke("set_modpack_update_notice", { instanceId, versionId });
  }
}
