/** Version labels noted when an update starts, until it finishes. */
export const pendingUpdateLabels = new Map<
  string,
  { from: string; to: string }
>();

/**
 * The name with the old version label swapped for the new one, or null when
 * there is nothing to suggest: no known old label, or the name does not carry it.
 */
export function suggestRenamedName(
  name: string,
  from: string | undefined,
  to: string | undefined
): string | null {
  if (!from || !to || from === to || !name.includes(from)) return null;
  const next = name.split(from).join(to).trim();
  return next && next !== name ? next : null;
}

/** Takes the labels noted for the update and turns them into a suggestion. */
export function takeRenameSuggestion(
  instanceId: string,
  name: string | undefined
): { version: string; suggested: string } | null {
  const labels = pendingUpdateLabels.get(instanceId);
  pendingUpdateLabels.delete(instanceId);
  if (!labels || name === undefined) return null;
  const suggested = suggestRenamedName(name, labels.from, labels.to);
  return suggested ? { version: labels.to, suggested } : null;
}
