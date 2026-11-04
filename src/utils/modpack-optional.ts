import { ModpackOptionalFile } from "@/models/modpack-update";

/** Keys of `files`, in list order, without duplicates. */
const keysOf = (files: ModpackOptionalFile[]): string[] =>
  Array.from(new Set(files.map((file) => file.key)));

export const selectAllOptional = (files: ModpackOptionalFile[]): string[] =>
  keysOf(files);

export const selectNoneOptional = (): string[] => [];

/** Checked files become unchecked and the other way round. */
export const invertOptional = (
  files: ModpackOptionalFile[],
  selected: string[]
): string[] => {
  const chosen = new Set(selected);
  return keysOf(files).filter((key) => !chosen.has(key));
};

export const toggleOptional = (selected: string[], key: string): string[] =>
  selected.includes(key)
    ? selected.filter((item) => item !== key)
    : [...selected, key];

/** Picks that still name a listed file, in list order. */
export const normalizeOptionalPicks = (
  files: ModpackOptionalFile[],
  selected: string[]
): string[] => {
  const chosen = new Set(selected);
  return keysOf(files).filter((key) => chosen.has(key));
};
