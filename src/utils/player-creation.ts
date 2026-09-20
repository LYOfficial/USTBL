export type PlayerCreationSource = "offline" | "vustb";

export function playerCreationSource(
  source: string
): PlayerCreationSource | undefined {
  if (source === "offline") return "offline";
  if (source === "https://www.ustb.world/skinapi/") return "vustb";
  return undefined;
}
