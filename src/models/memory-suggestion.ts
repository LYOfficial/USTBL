export type MemorySuggestionSource = "global" | "instance" | "auto";

/** A modpack's memory recommendation the player has not answered yet. */
export interface MemorySuggestion {
  instanceId: string;
  packName: string;
  /** What the pack recommends, in MB. */
  suggestedMb: number;
  /** What applying sets: the recommendation limited to the device. */
  cappedMb: number;
  totalMb: number;
  currentMb: number;
  currentSource: MemorySuggestionSource;
}
