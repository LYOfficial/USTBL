import { MemorySuggestion } from "@/models/memory-suggestion";

/** Identifies one question: the instance and the value being asked about. */
export const memorySuggestionKey = (suggestion: MemorySuggestion) =>
  `${suggestion.instanceId}:${suggestion.suggestedMb}`;

/**
 * The next suggestion to show. One is shown at a time; those the player
 * dismissed with "later" stay hidden until the next launch.
 */
export const nextMemorySuggestion = (
  suggestions: MemorySuggestion[],
  dismissed: ReadonlySet<string>
): MemorySuggestion | undefined =>
  suggestions.find(
    (suggestion) => !dismissed.has(memorySuggestionKey(suggestion))
  );

/** Memory is shown in MB, as on the game settings page. */
export const formatMemoryMb = (mb: number) => `${mb} MB`;

/** The recommendation was lowered to fit the device. */
export const isMemorySuggestionCapped = (suggestion: MemorySuggestion) =>
  suggestion.cappedMb < suggestion.suggestedMb;
