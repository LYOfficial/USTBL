import type { VustbTexture } from "@/models/vustb";

// Missing means unchanged; null means explicitly restore/clear.
export type OutfitDraft = Partial<Record<"skin" | "cape", VustbTexture | null>>;

export function outfitPreview(
  draft: OutfitDraft,
  current: { skin?: string; cape?: string; model?: string }
) {
  return {
    skin: draft.skin === undefined ? current.skin : draft.skin?.url,
    cape: draft.cape === undefined ? current.cape : draft.cape?.url,
    model: draft.skin === undefined ? current.model : draft.skin?.model,
  };
}
