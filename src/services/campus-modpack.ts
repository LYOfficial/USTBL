import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  CampusInstallOutcome,
  CampusInstallProgress,
  CampusModpackProject,
  CampusModpackRecommendation,
  CampusNewProject,
  CampusPublishFinished,
  CampusPublishProgress,
  CampusPublishStarted,
  CampusUnfinishedPublish,
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
  static async fetchCampusModpackRecommendation(
    slot: string
  ): Promise<InvokeResponse<CampusModpackRecommendation | null>> {
    return await invoke("fetch_campus_modpack_recommendation", { slot });
  }

  /**
   * Install the files of a campus modpack into the empty instance created for it.
   */
  @responseHandler("instance")
  static async installCampusModpack(
    instanceId: string
  ): Promise<InvokeResponse<CampusInstallOutcome>> {
    return await invoke("install_campus_modpack", { instanceId });
  }

  static onInstallProgress(
    callback: (payload: CampusInstallProgress) => void
  ): () => void {
    const unlisten = getCurrentWebview().listen<CampusInstallProgress>(
      "modpack-update:progress",
      (event) => callback(event.payload)
    );
    return () => {
      unlisten.then((removeListener) => removeListener());
    };
  }

  /**
   * Publishes to an existing project, or to a new one when `newProject` is
   * given (exactly one of the two).
   */
  @responseHandler("instance")
  static async publishCampusModpack(
    instanceId: string,
    project: string | null,
    newProject: CampusNewProject | null,
    versionNumber: string,
    changelog: string,
    files: string[]
  ): Promise<InvokeResponse<CampusPublishStarted>> {
    return await invoke("publish_campus_modpack", {
      instanceId,
      project,
      newProject,
      versionNumber,
      changelog,
      files,
    });
  }

  @responseHandler("instance")
  static async listCampusModpackPublishJobs(): Promise<
    InvokeResponse<CampusUnfinishedPublish[]>
  > {
    return await invoke("list_campus_modpack_publish_jobs");
  }

  @responseHandler("instance")
  static async resumeCampusModpackPublish(
    jobId: string
  ): Promise<InvokeResponse<CampusPublishStarted>> {
    return await invoke("resume_campus_modpack_publish", { jobId });
  }

  @responseHandler("instance")
  static async discardCampusModpackPublish(
    jobId: string
  ): Promise<InvokeResponse<null>> {
    return await invoke("discard_campus_modpack_publish", { jobId });
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
