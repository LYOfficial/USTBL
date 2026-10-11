import { invoke } from "@tauri-apps/api/core";
import { InvokeResponse } from "@/models/response";
import { responseHandler } from "@/utils/response";

export interface Ipv6Settings {
  bypass: string[];
}
export interface Ipv6Status {
  supported: boolean;
  elevated: boolean;
  installed: boolean;
  running: boolean;
  startedAt: number | null;
  error: string | null;
  settings: Ipv6Settings;
}
export interface Ipv6Node {
  name: string;
  latencyMs: number | null;
  selected: boolean;
}

export class Ipv6AccelerationService {
  @responseHandler("ipv6 acceleration")
  static async status(): Promise<InvokeResponse<Ipv6Status>> {
    return invoke("retrieve_ipv6_status");
  }

  @responseHandler("ipv6 acceleration")
  static async install(): Promise<InvokeResponse<Ipv6Status>> {
    return invoke("install_ipv6_core");
  }

  @responseHandler("ipv6 acceleration")
  static async start(
    settings: Ipv6Settings
  ): Promise<InvokeResponse<Ipv6Status>> {
    return invoke("start_ipv6_acceleration", { settings });
  }

  @responseHandler("ipv6 acceleration")
  static async stop(): Promise<InvokeResponse<Ipv6Status>> {
    return invoke("stop_ipv6_acceleration");
  }

  @responseHandler("ipv6 acceleration")
  static async nodes(test = false): Promise<InvokeResponse<Ipv6Node[]>> {
    return invoke("retrieve_ipv6_nodes", { test });
  }

  @responseHandler("ipv6 acceleration")
  static async select(name: string): Promise<InvokeResponse<void>> {
    return invoke("select_ipv6_node", { name });
  }
}
