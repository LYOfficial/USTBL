import type { InstanceGroups } from "@/models/config";
import type { InstanceSummary } from "@/models/instance/misc";
import { instanceNavigationGroups } from "@/utils/installing-instance";

export type SectionKind = "installing" | "installed" | "starred" | "custom";

/**
 * `title` is only filled for custom groups and game directories; the fixed
 * groups (installing, installed, starred) are titled by the caller from `key`.
 */
export interface Section {
  key: string;
  title: string;
  kind: SectionKind;
  instances: InstanceSummary[];
}

export interface SectionRef {
  key: string;
  kind: SectionKind;
}

export interface SidebarGameDir {
  name: string;
  title: string;
}

export interface DropResult {
  groups: InstanceGroups;
  /** Set when the drop adds (true) or removes (false) the instance's star. */
  starred?: boolean;
}

const INSTALLED_DIR_PREFIX = "installed:";

const hasOwn = (record: object, key: string) =>
  Object.prototype.hasOwnProperty.call(record, key);

export const emptyInstanceGroups = (): InstanceGroups => ({
  order: ["installed", "starred"],
  pinned: null,
  collapsed: [],
  custom: {},
});

/**
 * Fills whatever an older or hand-edited config lacks: fixed groups missing
 * from `order` are appended, unknown ids are dropped, a dangling `pinned` is
 * cleared.
 */
export const normalizeGroups = (
  groups?: Partial<InstanceGroups> | null
): InstanceGroups => {
  const custom = groups?.custom ?? {};
  const seen = new Set<string>();
  const order: string[] = [];
  for (const key of [
    ...(groups?.order ?? []),
    "installed",
    "starred",
    ...Object.keys(custom),
  ]) {
    if (seen.has(key)) continue;
    if (key === "installed" || key === "starred" || hasOwn(custom, key)) {
      seen.add(key);
      order.push(key);
    }
  }
  return {
    order,
    pinned:
      groups?.pinned && hasOwn(custom, groups.pinned) ? groups.pinned : null,
    collapsed: groups?.collapsed ?? [],
    custom,
  };
};

/** The key used in `order` for a section; game directories share "installed". */
export const sortKeyOf = (key: string) =>
  key.startsWith(INSTALLED_DIR_PREFIX) ? "installed" : key;

export const directorySectionKey = (name: string) =>
  `${INSTALLED_DIR_PREFIX}${name}`;

export const buildSidebar = (
  instances: InstanceSummary[],
  groups: InstanceGroups | undefined,
  navType: string,
  gameDirs: SidebarGameDir[]
): Section[] => {
  const g = normalizeGroups(groups);
  const byId = new Map(instances.map((instance) => [instance.id, instance]));
  const { installing, all: installed } = instanceNavigationGroups(instances);

  const customSection = (id: string): Section => ({
    key: id,
    title: g.custom[id].name,
    kind: "custom",
    instances: Array.from(new Set(g.custom[id].members ?? []))
      .map((member) => byId.get(member))
      .filter((instance): instance is InstanceSummary => !!instance),
  });

  const sections: Section[] = [];
  if (g.pinned) sections.push(customSection(g.pinned));
  if (installing.length > 0) {
    sections.push({
      key: "installing",
      title: "",
      kind: "installing",
      instances: installing,
    });
  }
  for (const key of g.order) {
    if (key === g.pinned) continue;
    if (key === "installed") {
      if (navType === "directory") {
        for (const dir of gameDirs) {
          const list = installed.filter(
            (instance) => instance.gameDir === dir.name
          );
          if (list.length > 0) {
            sections.push({
              key: directorySectionKey(dir.name),
              title: dir.title,
              kind: "installed",
              instances: list,
            });
          }
        }
      } else {
        sections.push({
          key: "installed",
          title: "",
          kind: "installed",
          instances: installed,
        });
      }
    } else if (key === "starred") {
      sections.push({
        key: "starred",
        title: "",
        kind: "starred",
        instances: installed.filter((instance) => instance.starred),
      });
    } else {
      sections.push(customSection(key));
    }
  }
  return sections;
};

export const isGroupNameTaken = (
  groups: InstanceGroups,
  name: string,
  exceptId?: string
) => {
  const wanted = name.trim().toLowerCase();
  return Object.entries(groups.custom).some(
    ([id, group]) =>
      id !== exceptId && group.name.trim().toLowerCase() === wanted
  );
};

export const createGroup = (
  groups: InstanceGroups,
  id: string,
  name: string
): InstanceGroups => {
  const g = normalizeGroups(groups);
  return {
    ...g,
    order: [...g.order, id],
    custom: { ...g.custom, [id]: { name: name.trim(), members: [] } },
  };
};

/** Returns the same object when the name is empty, taken or unchanged. */
export const renameGroup = (
  groups: InstanceGroups,
  id: string,
  name: string
): InstanceGroups => {
  const g = normalizeGroups(groups);
  const trimmed = name.trim();
  if (
    !hasOwn(g.custom, id) ||
    !trimmed ||
    trimmed === g.custom[id].name ||
    isGroupNameTaken(g, trimmed, id)
  ) {
    return groups;
  }
  return {
    ...g,
    custom: { ...g.custom, [id]: { ...g.custom[id], name: trimmed } },
  };
};

export const deleteGroup = (
  groups: InstanceGroups,
  id: string
): InstanceGroups => {
  const g = normalizeGroups(groups);
  if (!hasOwn(g.custom, id)) return groups;
  const custom = { ...g.custom };
  delete custom[id];
  return {
    order: g.order.filter((key) => key !== id),
    pinned: g.pinned === id ? null : g.pinned,
    collapsed: g.collapsed.filter((key) => key !== id),
    custom,
  };
};

/** At most one group is pinned: pinning another replaces it. */
export const togglePinned = (
  groups: InstanceGroups,
  id: string
): InstanceGroups => {
  const g = normalizeGroups(groups);
  if (!hasOwn(g.custom, id)) return groups;
  return { ...g, pinned: g.pinned === id ? null : id };
};

export const toggleCollapsed = (
  groups: InstanceGroups,
  key: string
): InstanceGroups => {
  const g = normalizeGroups(groups);
  return {
    ...g,
    collapsed: g.collapsed.includes(key)
      ? g.collapsed.filter((item) => item !== key)
      : [...g.collapsed, key],
  };
};

const addMember = (
  groups: InstanceGroups,
  id: string,
  instanceId: string
): InstanceGroups => {
  const group = groups.custom[id];
  if (!group || group.members.includes(instanceId)) return groups;
  return {
    ...groups,
    custom: {
      ...groups.custom,
      [id]: { ...group, members: [...group.members, instanceId] },
    },
  };
};

const removeMember = (
  groups: InstanceGroups,
  id: string,
  instanceId: string
): InstanceGroups => {
  const group = groups.custom[id];
  if (!group || !group.members.includes(instanceId)) return groups;
  return {
    ...groups,
    custom: {
      ...groups.custom,
      [id]: {
        ...group,
        members: group.members.filter((member) => member !== instanceId),
      },
    },
  };
};

/** Called after an instance is deleted; keeps unknown ids of other instances. */
export const removeInstanceFromGroups = (
  groups: InstanceGroups,
  instanceId: string
): InstanceGroups => {
  const g = normalizeGroups(groups);
  let next = g;
  for (const id of Object.keys(g.custom)) {
    next = removeMember(next, id, instanceId);
  }
  return next === g ? groups : next;
};

/**
 * Applies dropping an instance from `source` onto `target` (null = a spot in
 * the sidebar that is not a section). Returns null when nothing should change.
 * Automatic groups (installing, installed, directories) never lose an instance
 * and never accept one; "starred" is the instance's star.
 */
export const dropInstance = (
  groups: InstanceGroups,
  instanceId: string,
  source: SectionRef,
  target: SectionRef | null,
  copy: boolean
): DropResult | null => {
  const g = normalizeGroups(groups);
  if (target === null) {
    if (copy) return null;
    if (source.kind === "custom") {
      const next = removeMember(g, source.key, instanceId);
      return next === g ? null : { groups: next };
    }
    if (source.kind === "starred") return { groups: g, starred: false };
    return null;
  }
  if (target.kind === "installing" || target.kind === "installed") return null;
  if (target.key === source.key) return null;
  if (target.kind === "custom" && !hasOwn(g.custom, target.key)) return null;

  let next = g;
  let starred: boolean | undefined;
  if (target.kind === "starred") starred = true;
  else next = addMember(next, target.key, instanceId);
  if (!copy) {
    if (source.kind === "custom")
      next = removeMember(next, source.key, instanceId);
    else if (source.kind === "starred" && target.kind !== "starred") {
      starred = false;
    }
  }
  return { groups: next, starred };
};

/** Creates a group and drops the instance into it. Null if the name is unusable. */
export const addToNewGroup = (
  groups: InstanceGroups,
  newId: string,
  name: string,
  instanceId: string,
  source: SectionRef,
  copy: boolean
): DropResult | null => {
  const g = normalizeGroups(groups);
  if (!name.trim() || isGroupNameTaken(g, name)) return null;
  return dropInstance(
    createGroup(g, newId, name),
    instanceId,
    source,
    { key: newId, kind: "custom" },
    copy
  );
};

/** Whether a section can be reordered by dragging its title. */
export const isSortableSection = (
  groups: InstanceGroups,
  section: SectionRef
) =>
  section.kind !== "installing" &&
  !(
    section.kind === "custom" && normalizeGroups(groups).pinned === section.key
  );

/**
 * Moves a section next to another one in `order`. Both are section keys;
 * installing and the pinned group stay where they are and are ignored.
 * Returns the same object when nothing changes.
 */
export const moveSection = (
  groups: InstanceGroups,
  key: string,
  targetKey: string,
  after: boolean
): InstanceGroups => {
  const g = normalizeGroups(groups);
  const from = sortKeyOf(key);
  const to = sortKeyOf(targetKey);
  if (from === to || !g.order.includes(from) || !g.order.includes(to)) {
    return groups;
  }
  if (from === g.pinned || to === g.pinned) return groups;
  const rest = g.order.filter((item) => item !== from);
  rest.splice(rest.indexOf(to) + (after ? 1 : 0), 0, from);
  if (rest.every((item, index) => item === g.order[index])) return groups;
  return { ...g, order: rest };
};
