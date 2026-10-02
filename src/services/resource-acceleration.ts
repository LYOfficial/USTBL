import { invoke } from "@tauri-apps/api/core";
import { InvokeResponse } from "@/models/response";
import { responseHandler } from "@/utils/response";

export interface ResourceAccelerationStatus {
  running: boolean;
  loading: boolean;
  githubEnabled: boolean;
  hostsTotal: number;
  hostsCompleted: number;
  startedAt: number | null;
}

export interface ResourceAccelerationLatency {
  host: string;
  latencyMs: number | null;
  available: boolean;
}

export class ResourceAccelerationService {
  @responseHandler("resource acceleration")
  static async start(
    githubEnabled: boolean
  ): Promise<InvokeResponse<ResourceAccelerationStatus>> {
    return await invoke("start_resource_acceleration", {
      githubEnabled,
    });
  }

  @responseHandler("resource acceleration")
  static async stop(): Promise<InvokeResponse<ResourceAccelerationStatus>> {
    return await invoke("stop_resource_acceleration");
  }

  @responseHandler("resource acceleration")
  static async status(): Promise<InvokeResponse<ResourceAccelerationStatus>> {
    return await invoke("retrieve_resource_acceleration_status");
  }

  @responseHandler("resource acceleration")
  static async testLatency(
    githubEnabled: boolean
  ): Promise<InvokeResponse<ResourceAccelerationLatency[]>> {
    return await invoke("test_resource_acceleration_latency", {
      githubEnabled,
    });
  }
}
