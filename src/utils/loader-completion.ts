import { GTaskEventStatusEnums, TaskGroupDesc } from "@/models/task";

const loaderGroups = new Set([
  "forge-libraries",
  "neoforge-libraries",
  "optifine-libraries",
]);

/** One completion per instance/batch, after all its runtime groups succeeded.
 * Keep the claims outside React state updaters: React may replay those. */
export function takeLoaderCompletions(
  groups: TaskGroupDesc[],
  claimed: Map<string, Set<string>>,
  inFlight: ReadonlySet<string> = new Set()
): string[] {
  const instances = new Map<string, Map<string, TaskGroupDesc>>();
  for (const group of groups) {
    const at = group.taskGroup.lastIndexOf("@");
    const key = at < 0 ? group.taskGroup : group.taskGroup.slice(0, at);
    const separator = key.indexOf("?");
    const name = key.slice(0, separator);
    const id = key.slice(separator + 1);
    if (separator < 0 || !id || !loaderGroups.has(name)) continue;
    const latest = instances.get(id) ?? new Map<string, TaskGroupDesc>();
    const previous = latest.get(name);
    if (
      !previous ||
      Number(group.taskGroup.slice(at + 1)) >
        Number(
          previous.taskGroup.slice(previous.taskGroup.lastIndexOf("@") + 1)
        )
    ) {
      latest.set(name, group);
    }
    instances.set(id, latest);
  }
  const ready: string[] = [];
  for (const [id, latest] of instances) {
    if (inFlight.has(id)) continue;
    const batch = [...latest.values()];
    if (batch.some((g) => g.status !== GTaskEventStatusEnums.Completed))
      continue;
    const previous = claimed.get(id) ?? new Set<string>();
    if (batch.every((g) => previous.has(g.taskGroup))) continue;
    for (const group of batch) previous.add(group.taskGroup);
    claimed.set(id, previous);
    ready.push(id);
  }
  return ready;
}
