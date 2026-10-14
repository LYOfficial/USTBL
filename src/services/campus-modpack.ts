import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  CampusModpackProject,
  CampusPublishFinished,
  CampusPublishProgress,
  CampusPublishStarted,
} from "@/models/campus-modpack";
import { InvokeResponse } from "@/models/response";
import { responseHandler } from "@/utils/response";

export class CampusModpackService {
  @responseHandler("instance")
  static async listCampusModpackProjects(): Promise<
    InvokeResponse<CampusModpackProject[]>
  > {
    return await invoke("list_campus_modpack_projects");
  }

  @responseHandler("instance")
  static async createCampusModpackProject(
    slug: string,
    name: string,
    summary: string
  ): Promise<InvokeResponse<CampusModpackProject>> {
    return await invoke("create_campus_modpack_project", {
      slug,
      name,
      summary,
    });
  }

  @responseHandler("instance")
  static async publishCampusModpack(
    instanceId: string,
    project: string,
    versionNumber: string,
    changelog: string,
    files: string[]
  ): Promise<InvokeResponse<CampusPublishStarted>> {
    return await invoke("publish_campus_modpack", {
      instanceId,
      project,
      versionNumber,
      changelog,
      files,
    });
  }

  /**
   * Paths the line's current release already lists; empty when it has none.
   */
  @responseHandler("instance")
  static async fetchCampusModpackPreviousPaths(
    instanceId: string,
    project: string
  ): Promise<InvokeResponse<string[]>> {
    return await invoke("fetch_campus_modpack_previous_paths", {
      instanceId,
      project,
    });
  }

  static onPublishFinished(
    callback: (payload: CampusPublishFinished) => void
  ): () => void {
    const unlisten = getCurrentWebview().listen<CampusPublishFinished>(
      "modpack-publish:finished",
      (event) => callback(event.payload)
    );
    return () => {
      unlisten.then((removeListener) => removeListener());
    };
  }

  static onPublishProgress(
    callback: (payload: CampusPublishProgress) => void
  ): () => void {
    const unlisten = getCurrentWebview().listen<CampusPublishProgress>(
      "modpack-publish:progress",
      (event) => callback(event.payload)
    );
    return () => {
      unlisten.then((removeListener) => removeListener());
    };
  }
}
