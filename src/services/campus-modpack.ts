import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  CampusInstallFinished,
  CampusInstallProgress,
  CampusInstallStarted,
  CampusModpackProject,
  CampusModpackRecommendation,
  CampusNewProject,
  CampusPublishFile,
  CampusPublishFinished,
  CampusPublishProgress,
  CampusPublishRule,
  CampusPublishStarted,
  CampusPublishTree,
  CampusUnfinishedInstall,
  CampusUnfinishedPublish,
} from "@/models/campus-modpack";
import { ModpackDiffHunk } from "@/models/modpack-update";
import { InvokeResponse } from "@/models/response";
import { responseHandler } from "@/utils/response";

export class CampusModpackService {
  @responseHandler("instance")
  static async listCampusModpackProjects(): Promise<
    InvokeResponse<CampusModpackProject[]>
  > {
    return await invoke("list_campus_modpack_projects");
  }

  /**
   * Remove a campus modpack (every line and version) from the platform.
   * Administrators only.
   */
  @responseHandler("instance")
  static async deleteCampusModpackProject(
    project: string
  ): Promise<InvokeResponse<null>> {
    return await invoke("delete_campus_modpack_project", { project });
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
  ): Promise<InvokeResponse<CampusInstallStarted>> {
    return await invoke("install_campus_modpack", { instanceId });
  }

  @responseHandler("instance")
  static async registerCampusModpackInstall(
    instanceId: string,
    label: string
  ): Promise<InvokeResponse<null>> {
    return await invoke("register_campus_modpack_install", {
      instanceId,
      label,
    });
  }

  @responseHandler("instance")
  static async listCampusModpackInstallJobs(): Promise<
    InvokeResponse<CampusUnfinishedInstall[]>
  > {
    return await invoke("list_campus_modpack_install_jobs");
  }

  @responseHandler("instance")
  static async resumeCampusModpackInstall(
    instanceId: string
  ): Promise<InvokeResponse<CampusInstallStarted>> {
    return await invoke("resume_campus_modpack_install", { instanceId });
  }

  @responseHandler("instance")
  static async discardCampusModpackInstall(
    instanceId: string
  ): Promise<InvokeResponse<null>> {
    return await invoke("discard_campus_modpack_install", { instanceId });
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

  static onInstallFinished(
    callback: (payload: CampusInstallFinished) => void
  ): () => void {
    const unlisten = getCurrentWebview().listen<CampusInstallFinished>(
      "modpack-install:finished",
      (event) => callback(event.payload)
    );
    return () => {
      unlisten.then((removeListener) => removeListener());
    };
  }

  static onInstallGroupsRemoved(
    callback: (groups: string[]) => void
  ): () => void {
    const unlisten = getCurrentWebview().listen<string[]>(
      "modpack-install:groups-removed",
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
    files: CampusPublishFile[],
    rules: CampusPublishRule[]
  ): Promise<InvokeResponse<CampusPublishStarted>> {
    return await invoke("publish_campus_modpack", {
      instanceId,
      project,
      newProject,
      versionNumber,
      changelog,
      files,
      rules,
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
   * The publish tree to open with: the instance's files, starting from what
   * the project line's current release lists, and how each file differs from
   * that release. `project` is null for a new project. Failing to read the
   * release is an error, never "no release".
   */
  @responseHandler("instance")
  static async prepareCampusModpackPublishTree(
    instanceId: string,
    project: string | null
  ): Promise<InvokeResponse<CampusPublishTree>> {
    return await invoke("prepare_campus_modpack_publish_tree", {
      instanceId,
      project,
    });
  }

  /**
   * The line diff of a file from the previous release's text to the file on
   * disk. `null` when the previous text is not kept locally or the file is not
   * text; empty when the texts only differ in line endings and the like.
   */
  @responseHandler("instance")
  static async diffCampusModpackPublishFile(
    instanceId: string,
    path: string,
    previousSha1: string,
    previousVersionId: string,
    previousProjectId: string
  ): Promise<InvokeResponse<ModpackDiffHunk[] | null>> {
    return await invoke("diff_campus_modpack_publish_file", {
      instanceId,
      path,
      previousSha1,
      previousVersionId,
      previousProjectId,
    });
  }

  /**
   * Gate the publish dialog on open: fails when the instance was imported by
   * an older launcher version and lacks the component data for publishing.
   */
  @responseHandler("instance")
  static async checkCampusModpackPublishable(
    instanceId: string
  ): Promise<InvokeResponse<null>> {
    return await invoke("check_campus_modpack_publishable", { instanceId });
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
