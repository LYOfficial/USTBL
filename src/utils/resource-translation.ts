interface TranslatableResource {
  id?: string;
  translatedDescription?: string;
}

/**
 * Hands out increasing tokens so a late reply can tell whether the list it was
 * requested for is still the one on screen.
 */
export const createRequestGate = () => {
  let current = 0;
  return {
    next: (): number => {
      current += 1;
      return current;
    },
    current: (): number => current,
    isCurrent: (token: number): boolean => token === current,
  };
};

/** Ids of the resources that still need a translated description. */
export const pendingTranslationIds = (list: TranslatableResource[]) => {
  const ids = new Set<string>();
  list.forEach((item) => {
    if (item.id && !item.translatedDescription) ids.add(item.id);
  });
  return Array.from(ids);
};

interface SourcedTranslatableResource extends TranslatableResource {
  source?: string;
}

/**
 * Ids that still need a translated description, grouped by download source,
 * for lists that mix sources.
 */
export const pendingTranslationIdsBySource = (
  list: SourcedTranslatableResource[]
): Record<string, string[]> => {
  const groups: Record<string, string[]> = {};
  list.forEach((item) => {
    if (!item.id || !item.source || item.translatedDescription) return;
    const ids = (groups[item.source] ??= []);
    if (!ids.includes(item.id)) ids.push(item.id);
  });
  return groups;
};

/**
 * Like `mergeTranslations`, but only touches items of the given source, since
 * ids of different sources may collide.
 */
export const mergeTranslationsForSource = <
  T extends SourcedTranslatableResource,
>(
  list: T[],
  source: string,
  translations: Record<string, string>
): T[] => {
  let changed = false;
  const merged = list.map((item) => {
    const text =
      item.id && item.source === source ? translations[item.id] : undefined;
    if (!text || item.translatedDescription === text) return item;
    changed = true;
    return { ...item, translatedDescription: text };
  });
  return changed ? merged : list;
};

/**
 * Put translations (id -> text) into the list. Returns the same array when
 * nothing changes, so a state update with it does not re-render.
 */
export const mergeTranslations = <T extends TranslatableResource>(
  list: T[],
  translations: Record<string, string>
): T[] => {
  let changed = false;
  const merged = list.map((item) => {
    const text = item.id ? translations[item.id] : undefined;
    if (!text || item.translatedDescription === text) return item;
    changed = true;
    return { ...item, translatedDescription: text };
  });
  return changed ? merged : list;
};
