import { invoke } from "@tauri-apps/api/core";
import { MemorySuggestion } from "@/models/memory-suggestion";
import { InvokeResponse } from "@/models/response";
import { responseHandler } from "@/utils/response";

export class MemorySuggestionService {
  /** The suggestions to ask the player about now, over all instances. */
  @responseHandler("instance")
  static async listMemorySuggestions(): Promise<
    InvokeResponse<MemorySuggestion[]>
  > {
    return await invoke("list_memory_suggestions");
  }

  /**
   * Answer the suggestion for `suggestedMb`: apply it to the instance, or keep
   * the current memory. Either way the same value is not asked again.
   */
  @responseHandler("instance")
  static async answerMemorySuggestion(
    instanceId: string,
    suggestedMb: number,
    apply: boolean
  ): Promise<InvokeResponse<null>> {
    return await invoke("answer_memory_suggestion", {
      instanceId,
      suggestedMb,
      apply,
    });
  }
}
