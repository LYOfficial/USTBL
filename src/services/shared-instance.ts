import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { InvokeResponse } from "@/models/response";
import {
  SharedInstance,
  SharedInstanceDetail,
  SharedInstanceStartupNotification,
  SharedMod,
  SharedUpdateProgress,
  SharedUpdateResult,
} from "@/models/shared-instance";
import { responseHandler } from "@/utils/response";

export class SharedInstanceService {
  @responseHandler("instance")
  static async retrieveList(): Promise<InvokeResponse<SharedInstance[]>> {
    return await invoke("retrieve_shared_instance_list");
  }

  @responseHandler("instance")
  static async retrieveDetail(
    sharedInstanceId: number
  ): Promise<InvokeResponse<SharedInstanceDetail>> {
    return await invoke("retrieve_shared_instance_detail", {
      sharedInstanceId,
    });
  }

  @responseHandler("instance")
  static async retrieveStartupNotifications(): Promise<
    InvokeResponse<SharedInstanceStartupNotification[]>
  > {
    return await invoke("retrieve_shared_instance_startup_notifications");
  }

  @responseHandler("instance")
  static async retrieveBinding(
    sharedInstanceId: number
  ): Promise<InvokeResponse<string | null>> {
    return await invoke("retrieve_shared_instance_binding", {
      sharedInstanceId,
    });
  }

  @responseHandler("instance")
  static async setBinding(
    sharedInstanceId: number,
    localInstanceId: string
  ): Promise<InvokeResponse<void>> {
    return await invoke("set_shared_instance_binding", {
      sharedInstanceId,
      localInstanceId,
    });
  }

  @responseHandler("instance")
  static async ignoreBindingPrompt(
    sharedInstanceId: number
  ): Promise<InvokeResponse<void>> {
    return await invoke("ignore_shared_instance_binding_prompt", {
      sharedInstanceId,
    });
  }

  @responseHandler("instance")
  static async update(
    sharedInstanceId: number,
    localInstanceId: string
  ): Promise<InvokeResponse<SharedUpdateResult>> {
    return await invoke("update_shared_instance", {
      sharedInstanceId,
      localInstanceId,
    });
  }

  static onUpdateProgress(
    callback: (payload: SharedUpdateProgress) => void
  ): () => void {
    const unlisten = getCurrentWebview().listen<SharedUpdateProgress>(
      "shared-instance:update-progress",
      (event) => callback(event.payload)
    );
    return () => {
      unlisten.then((removeListener) => removeListener());
    };
  }

  @responseHandler("instance")
  static async uploadMod(
    sharedInstanceId: number,
    filePath: string,
    folderId: number | null
  ): Promise<InvokeResponse<SharedMod>> {
    return await invoke("upload_shared_instance_mod", {
      sharedInstanceId,
      filePath,
      folderId,
    });
  }

  @responseHandler("instance")
  static async updateMod(
    sharedInstanceId: number,
    sharedModId: number,
    filePath: string
  ): Promise<InvokeResponse<SharedMod>> {
    return await invoke("update_shared_instance_mod", {
      sharedInstanceId,
      sharedModId,
      filePath,
    });
  }

  @responseHandler("instance")
  static async deleteMod(
    sharedInstanceId: number,
    sharedModId: number
  ): Promise<InvokeResponse<SharedMod>> {
    return await invoke("delete_shared_instance_mod", {
      sharedInstanceId,
      sharedModId,
    });
  }
}
