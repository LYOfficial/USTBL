/**
 * The local mod list is returned before its Chinese names and descriptions are
 * known; the backend sends them afterwards as updates keyed by jar path.
 */
export interface LocalModUpdate {
  filePath: string;
  translatedName?: string | null;
  translatedDescription?: string | null;
}

export interface LocalModUpdatesPayload {
  instanceId: string;
  mods: LocalModUpdate[];
}

interface UpdatableMod {
  filePath: string;
  translatedName?: string;
  translatedDescription?: string;
}

/**
 * Apply updates to a mod list. An update replaces both texts (a missing one
 * clears the old value). The same array is returned when nothing changes, so
 * state setters can skip the render.
 */
export const mergeLocalModUpdates = <T extends UpdatableMod>(
  list: T[],
  updates: Iterable<LocalModUpdate> | ReadonlyMap<string, LocalModUpdate>
): T[] => {
  const byPath =
    updates instanceof Map
      ? updates
      : new Map(
          Array.from(updates as Iterable<LocalModUpdate>, (update) => [
            update.filePath,
            update,
          ])
        );
  if (byPath.size === 0) return list;

  let changed = false;
  const merged = list.map((mod) => {
    const update = byPath.get(mod.filePath);
    if (!update) return mod;
    const translatedName = update.translatedName ?? undefined;
    const translatedDescription = update.translatedDescription ?? undefined;
    if (
      mod.translatedName === translatedName &&
      mod.translatedDescription === translatedDescription
    ) {
      return mod;
    }
    changed = true;
    return { ...mod, translatedName, translatedDescription };
  });
  return changed ? merged : list;
};

/**
 * Collects the updates that arrive for one instance, so a list that finishes
 * loading after its updates were sent can still apply them. Updates for any
 * other instance are ignored.
 */
export const createLocalModUpdateBuffer = () => {
  let instanceId: string | undefined;
  let updates = new Map<string, LocalModUpdate>();
  return {
    /** Start collecting for `id`, dropping everything collected before. */
    reset: (id: string | undefined) => {
      instanceId = id;
      updates = new Map();
    },
    /** Returns whether the payload belongs to the instance being collected. */
    add: (payload: LocalModUpdatesPayload): boolean => {
      if (instanceId === undefined || payload.instanceId !== instanceId) {
        return false;
      }
      payload.mods.forEach((update) => updates.set(update.filePath, update));
      return true;
    },
    updates: (): ReadonlyMap<string, LocalModUpdate> => updates,
  };
};
