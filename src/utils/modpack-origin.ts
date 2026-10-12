import { ModpackOriginInput } from "@/models/instance/misc";

/**
 * Origins claimed by modpack downloads that have been scheduled but not yet
 * imported, keyed by destination path. Kept in memory only: if the launcher
 * restarts before the import, the instance simply gets no origin.
 */
const pending = new Map<string, ModpackOriginInput>();

const keyOf = (dest: string) => dest.replace(/\\/g, "/").toLowerCase();

export const registerPendingModpackOrigin = (
  dest: string,
  origin: ModpackOriginInput
) => {
  pending.set(keyOf(dest), origin);
};

export const takePendingModpackOrigin = (
  dest: string
): ModpackOriginInput | undefined => {
  const key = keyOf(dest);
  const origin = pending.get(key);
  pending.delete(key);
  return origin;
};
