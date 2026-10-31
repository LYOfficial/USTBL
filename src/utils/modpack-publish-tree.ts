// Pure logic behind the publish dialog's tri-state file tree (plan 《整合包发布
// 三态与本地忽略》§4). The tree is a view over `PublishDraft`: a list of rules
// kept exactly as the publisher set them, plus the files that are ticked
// explicitly where a rule would otherwise cover them.

export type PublishState = "include" | "ignore" | "delete";
/** What a node can show: a virtual file with no rule is "remove" (unmentioned). */
export type ShownState = PublishState | "remove" | "mixed";

export interface PublishRule {
  path: string;
  kind: "file" | "dir";
  state: "ignore" | "delete";
}

export interface PublishFileEntry {
  /** Always the enabled name. */
  path: string;
  disabled: boolean;
  once: boolean;
}

export interface DiskFile {
  /** The enabled name. */
  path: string;
  disabled: boolean;
  /** Both `<path>` and `<path>.disabled` are on disk. */
  conflict: boolean;
}

/** A path the previous release mentions that is not on disk any more. */
export interface GhostEntry {
  path: string;
  kind: "file" | "dir";
}

export interface PublishContext {
  files: DiskFile[];
  ghosts: GhostEntry[];
}

export interface PublishDraft {
  /** Saved as is, in this order. */
  rules: PublishRule[];
  /** Keys of files ticked explicitly, which only matters under a rule. */
  included: Set<string>;
  /** Keys of files that install only once. */
  once: Set<string>;
}

export interface PublishPayload {
  files: PublishFileEntry[];
  rules: PublishRule[];
  /** Ticked files whose enabled and disabled variants are both on disk. */
  conflicts: string[];
}

export const MAX_RULES = 2000;

/** Matches the lowercased keys the launcher's starting state uses. */
export const keyOf = (path: string): string => path.toLowerCase();

const isUnder = (key: string, dirKey: string): boolean =>
  key.startsWith(`${dirKey}/`);

interface RuleIndex {
  files: Map<string, PublishRule>;
  dirs: Map<string, PublishRule>;
}

const indexRules = (rules: PublishRule[]): RuleIndex => {
  const index: RuleIndex = { files: new Map(), dirs: new Map() };
  for (const rule of rules) {
    (rule.kind === "file" ? index.files : index.dirs).set(
      keyOf(rule.path),
      rule
    );
  }
  return index;
};

/** The deepest dir rule strictly above a path. */
const coveringRule = (
  index: RuleIndex,
  key: string
): PublishRule | undefined => {
  let cut = key.lastIndexOf("/");
  while (cut > 0) {
    const rule = index.dirs.get(key.slice(0, cut));
    if (rule) return rule;
    cut = key.lastIndexOf("/", cut - 1);
  }
  return undefined;
};

export interface PublishView {
  fileState: (path: string) => ShownState;
  dirState: (path: string) => ShownState;
  /** A rule or explicit tick sits on this node, so "restore inheritance" applies. */
  hasOverride: (path: string, kind: "file" | "dir") => boolean;
  isOnce: (path: string) => boolean;
}

/** Read-only lookups over a draft; build once per render. */
export const viewOf = (
  context: PublishContext,
  draft: PublishDraft
): PublishView => {
  const index = indexRules(draft.rules);
  const diskKeys = new Set(context.files.map((file) => keyOf(file.path)));

  const fileStateOf = (key: string): ShownState => {
    const real = diskKeys.has(key);
    if (real && draft.included.has(key)) return "include";
    const own = index.files.get(key);
    if (own) return own.state;
    const covering = coveringRule(index, key);
    if (covering) return covering.state;
    return real ? "include" : "remove";
  };

  let summaries: Map<string, Map<ShownState, number>> | null = null;
  const summaryOf = (dirKey: string) => {
    if (!summaries) {
      summaries = new Map();
      for (const file of context.files) {
        const key = keyOf(file.path);
        const state = fileStateOf(key);
        let cut = key.lastIndexOf("/");
        while (cut > 0) {
          const parent = key.slice(0, cut);
          let counts = summaries.get(parent);
          if (!counts) {
            counts = new Map();
            summaries.set(parent, counts);
          }
          counts.set(state, (counts.get(state) ?? 0) + 1);
          cut = key.lastIndexOf("/", cut - 1);
        }
      }
    }
    return summaries.get(dirKey);
  };

  return {
    fileState: (path) => fileStateOf(keyOf(path)),
    dirState: (path) => {
      const key = keyOf(path);
      const counts = summaryOf(key);
      if (!counts || counts.size === 0) {
        const own = index.dirs.get(key) ?? coveringRule(index, key);
        return own ? own.state : "remove";
      }
      return counts.size === 1 ? Array.from(counts.keys())[0] : "mixed";
    },
    hasOverride: (path, kind) => {
      const key = keyOf(path);
      return kind === "dir"
        ? index.dirs.has(key)
        : index.files.has(key) || draft.included.has(key);
    },
    isOnce: (path) => draft.once.has(keyOf(path)),
  };
};

/** Files present on the publisher's disk that a delete state will remove. */
export const localDeletePaths = (
  context: PublishContext,
  draft: PublishDraft
): string[] => {
  const view = viewOf(context, draft);
  return context.files
    .filter((file) => view.fileState(file.path) === "delete")
    .map((file) => file.path);
};

const withRule = (rules: PublishRule[], next: PublishRule): PublishRule[] => {
  const key = keyOf(next.path);
  const at = rules.findIndex((rule) => keyOf(rule.path) === key);
  if (at < 0) return [...rules, next];
  const copy = rules.slice();
  copy[at] = next;
  return copy;
};

const withoutRules = (
  rules: PublishRule[],
  drop: (key: string, rule: PublishRule) => boolean
): PublishRule[] => rules.filter((rule) => !drop(keyOf(rule.path), rule));

const isGhostFile = (context: PublishContext, key: string): boolean =>
  !context.files.some((file) => keyOf(file.path) === key);

/** A file takes `state`; a rule appears only where it differs from what the file inherits. */
export const setFileState = (
  context: PublishContext,
  draft: PublishDraft,
  path: string,
  state: PublishState | "remove"
): PublishDraft => {
  const key = keyOf(path);
  const ghost = isGhostFile(context, key);
  let rules = withoutRules(
    draft.rules,
    (ruleKey, rule) => rule.kind === "file" && ruleKey === key
  );
  const included = new Set(draft.included);
  included.delete(key);
  const covering = coveringRule(indexRules(rules), key);
  const inherited: ShownState = covering
    ? covering.state
    : ghost
      ? "remove"
      : "include";
  if (!ghost && state === "include") {
    if (inherited !== "include") included.add(key);
  } else if (state === "ignore" || state === "delete") {
    if (inherited !== state)
      rules = withRule(rules, { path, kind: "file", state });
  }
  return { rules, included, once: draft.once };
};

/**
 * Set the state of a directory by changing its own rule only; settings on
 * nodes below it are the publisher's deliberate overrides and stay. Ignore and
 * delete leave a rule on the directory unless it inherits that state already.
 * Include has no rule of its own, so the directory's rule is dropped and the
 * files that would still inherit a non-include state from outside the
 * directory are ticked; files with their own file rule or governed by a deeper
 * directory rule are left alone.
 */
export const setDirState = (
  context: PublishContext,
  draft: PublishDraft,
  path: string,
  state: PublishState
): PublishDraft => {
  const dirKey = keyOf(path);
  const without = withoutRules(
    draft.rules,
    (key, rule) => key === dirKey && rule.kind === "dir"
  );
  const index = indexRules(without);
  const included = new Set(draft.included);
  if (state === "include") {
    context.files.forEach((file) => {
      const key = keyOf(file.path);
      if (!isUnder(key, dirKey) || index.files.has(key)) return;
      const covering = coveringRule(index, key);
      if (covering && !isUnder(keyOf(covering.path), dirKey)) {
        included.add(key);
      }
    });
    return { rules: without, included, once: draft.once };
  }
  const covering = coveringRule(index, dirKey);
  const inherited: ShownState = covering ? covering.state : "include";
  return {
    rules:
      inherited !== state
        ? withRule(draft.rules, { path, kind: "dir", state })
        : without,
    included,
    once: draft.once,
  };
};
/**
 * Drop the setting on this node so it inherits again. A file goes back to its
 * covering rule (or ticked); a directory loses its own rule only.
 */
export const restoreInheritance = (
  draft: PublishDraft,
  path: string,
  kind: "file" | "dir"
): PublishDraft => {
  const key = keyOf(path);
  const included = new Set(draft.included);
  included.delete(key);
  return {
    rules: withoutRules(
      draft.rules,
      (ruleKey, rule) => rule.kind === kind && ruleKey === key
    ),
    included,
    once: draft.once,
  };
};

export const setOnce = (
  draft: PublishDraft,
  path: string,
  value: boolean
): PublishDraft => {
  const once = new Set(draft.once);
  if (value) once.add(keyOf(path));
  else once.delete(keyOf(path));
  return { ...draft, once };
};

/**
 * The state a click moves to; a mixed node goes to the first. A node missing
 * from disk (`ghost`) can never be ticked: it cycles remove, ignore, delete, or
 * only ignore, delete once a directory rule covers it.
 */
export const nextState = (
  current: ShownState,
  options: { canRemove: boolean; ghost?: boolean }
): PublishState | "remove" => {
  const order: (PublishState | "remove")[] = options.canRemove
    ? ["remove", "ignore", "delete"]
    : options.ghost
      ? ["ignore", "delete"]
      : ["include", "ignore", "delete"];
  const at = order.indexOf(current as PublishState | "remove");
  return order[(at + 1) % order.length];
};

/** Whether a removed state can be picked: not under a rule that covers the node. */
export const canRemoveFile = (draft: PublishDraft, path: string): boolean =>
  coveringRule(indexRules(draft.rules), keyOf(path)) === undefined;

export const toPublishPayload = (
  context: PublishContext,
  draft: PublishDraft
): PublishPayload => {
  const view = viewOf(context, draft);
  const files: PublishFileEntry[] = [];
  const conflicts: string[] = [];
  for (const file of context.files) {
    if (view.fileState(file.path) !== "include") continue;
    files.push({
      path: file.path,
      disabled: file.disabled,
      once: view.isOnce(file.path),
    });
    if (file.conflict) conflicts.push(file.path);
  }
  return {
    files,
    rules: draft.rules.map((rule) => ({ ...rule })),
    conflicts,
  };
};

export interface PublishNode {
  path: string;
  name: string;
  kind: "file" | "dir";
  /** Not on disk: a file the previous release lists, or a rule's target. */
  ghost: boolean;
  disabled: boolean;
  conflict: boolean;
  children: PublishNode[];
}

/** Directories first, then files, each by name. */
export const buildPublishNodes = (context: PublishContext): PublishNode[] => {
  const roots: PublishNode[] = [];
  const byKey = new Map<string, PublishNode>();
  const place = (
    path: string,
    leaf: Pick<PublishNode, "kind" | "ghost" | "disabled" | "conflict">
  ) => {
    const parts = path.split("/").filter(Boolean);
    let siblings = roots;
    let current = "";
    parts.forEach((part, at) => {
      current = current ? `${current}/${part}` : part;
      const key = keyOf(current);
      let node = byKey.get(key);
      const last = at === parts.length - 1;
      if (!node) {
        node = {
          path: current,
          name: part,
          kind: last ? leaf.kind : "dir",
          ghost: last ? leaf.ghost : true,
          disabled: last ? leaf.disabled : false,
          conflict: last ? leaf.conflict : false,
          children: [],
        };
        byKey.set(key, node);
        siblings.push(node);
      } else if (last && node.kind === "dir" && leaf.kind === "dir") {
        node.ghost = node.ghost && leaf.ghost;
      }
      if (!last && !leaf.ghost) node.ghost = false;
      siblings = node.children;
    });
  };
  for (const file of context.files) {
    place(file.path, {
      kind: "file",
      ghost: false,
      disabled: file.disabled,
      conflict: file.conflict,
    });
  }
  for (const ghost of context.ghosts) {
    place(ghost.path, {
      kind: ghost.kind,
      ghost: true,
      disabled: false,
      conflict: false,
    });
  }
  const sort = (nodes: PublishNode[]) => {
    nodes.sort((a, b) =>
      a.kind !== b.kind
        ? a.kind === "dir"
          ? -1
          : 1
        : a.name.localeCompare(b.name)
    );
    nodes.forEach((node) => sort(node.children));
  };
  sort(roots);
  return roots;
};

// ------------------------------------------------------------- change marks
//
// How the files differ from the project line's current release (plan 《整合包
// 本地改动明细》§11.3). The launcher lists the content differences; deleted
// files are the tree's own ghosts.

export type ChangeKind = "modified" | "added" | "toggled" | "onceChanged";
/** A kind of mark; "deleted" is a file of the previous release gone from disk. */
export type ChangeCategory = ChangeKind | "deleted";

export const CHANGE_CATEGORIES: ChangeCategory[] = [
  "modified",
  "added",
  "toggled",
  "onceChanged",
  "deleted",
];

export interface PublishChange {
  /** The enabled name. */
  path: string;
  kind: ChangeKind;
  /** The previous release's content; null for an added file. */
  previousSha1: string | null;
  previousSize: number | null;
  localSize: number | null;
  /** The player-side ignore list names the file; publishing does not care. */
  localIgnored: boolean;
  /** Backend-verified location of the previous release. */
  previousVersionId?: string | null;
  previousProjectId?: string | null;
}

export interface PreviousFile {
  path: string;
  sha1: string;
  size: number;
  disabled: boolean;
  once: boolean;
}

/** What the project line's current release lists; null for a new project. */
export interface PreviousRelease {
  files: PreviousFile[];
  rules: PublishRule[];
  rulesManifest: boolean;
}

/**
 * The mark of each node by lowercased path. A file the previous release lacks
 * is only "added" while the draft publishes it.
 */
export const categorizeChanges = (
  changes: PublishChange[],
  ghosts: GhostEntry[],
  previous: PreviousRelease | null,
  view: PublishView
): Map<string, ChangeCategory> => {
  const marks = new Map<string, ChangeCategory>();
  for (const change of changes) {
    if (change.kind === "added" && view.fileState(change.path) !== "include") {
      continue;
    }
    marks.set(keyOf(change.path), change.kind);
  }
  if (previous) {
    const listed = new Set(previous.files.map((file) => keyOf(file.path)));
    for (const ghost of ghosts) {
      if (ghost.kind === "file" && listed.has(keyOf(ghost.path))) {
        marks.set(keyOf(ghost.path), "deleted");
      }
    }
  }
  return marks;
};

export const countCategories = (
  marks: Map<string, ChangeCategory>
): Record<ChangeCategory, number> => {
  const counts: Record<ChangeCategory, number> = {
    modified: 0,
    added: 0,
    toggled: 0,
    onceChanged: 0,
    deleted: 0,
  };
  marks.forEach((category) => {
    counts[category] += 1;
  });
  return counts;
};

/** How many marked files sit under each directory, by lowercased path. */
export const dirChangeCounts = (
  marks: Map<string, ChangeCategory>
): Map<string, number> => {
  const counts = new Map<string, number>();
  marks.forEach((_, key) => {
    let cut = key.lastIndexOf("/");
    while (cut > 0) {
      const parent = key.slice(0, cut);
      counts.set(parent, (counts.get(parent) ?? 0) + 1);
      cut = key.lastIndexOf("/", cut - 1);
    }
  });
  return counts;
};

export interface ChangeFilter {
  /** Show only these marks; empty means every mark. */
  categories: ReadonlySet<ChangeCategory>;
  /** Show only marked files. */
  onlyChanged: boolean;
}

export const NO_FILTER: ChangeFilter = {
  categories: new Set(),
  onlyChanged: false,
};

export const isFilterActive = (filter: ChangeFilter): boolean =>
  filter.onlyChanged || filter.categories.size > 0;

export const toggleCategory = (
  filter: ChangeFilter,
  category: ChangeCategory
): ChangeFilter => {
  const categories = new Set(filter.categories);
  if (categories.has(category)) categories.delete(category);
  else categories.add(category);
  return { ...filter, categories };
};

/**
 * The tree with only the files the filter lets through, and the directories
 * that lead to them. The tree itself is returned when no filter is active.
 */
export const filterNodes = (
  nodes: PublishNode[],
  marks: Map<string, ChangeCategory>,
  filter: ChangeFilter
): PublishNode[] => {
  if (!isFilterActive(filter)) return nodes;
  const keep = (list: PublishNode[]): PublishNode[] =>
    list.flatMap((node) => {
      if (node.kind === "file") {
        const category = marks.get(keyOf(node.path));
        const shown =
          category !== undefined &&
          (filter.categories.size === 0 || filter.categories.has(category));
        return shown ? [node] : [];
      }
      const children = keep(node.children);
      return children.length > 0 ? [{ ...node, children }] : [];
    });
  return keep(nodes);
};

/** The slug of the project to publish to by default, or null for a new one. */
export const defaultPublishProject = (
  origin: { platform: string; projectId: string } | null | undefined,
  projects: { slug: string }[]
): string | null => {
  if (!origin || origin.platform !== "campus") return null;
  return projects.some((project) => project.slug === origin.projectId)
    ? origin.projectId
    : null;
};

/**
 * The target to show once the project list is in. A choice the user made
 * (a slug, or null for a new project) stays; `undefined` means none yet.
 */
export const publishTargetAfterLoad = (
  userChoice: string | null | undefined,
  origin: { platform: string; projectId: string } | null | undefined,
  projects: { slug: string }[]
): string | null =>
  userChoice !== undefined
    ? userChoice
    : defaultPublishProject(origin, projects);

export interface PublishSummary {
  /** Nothing to compare with: every pushed file is new. */
  isNew: boolean;
  /** Files this version lists. */
  pushed: number;
  added: string[];
  /** Content differs from the previous release. */
  modified: string[];
  /** Content differs, and the file stays first-install only. */
  onceUpdated: string[];
  /** Listed before and not pushed now, and nothing else mentions it. */
  removed: string[];
  /** Listed before, now ignored. */
  ignored: string[];
  /** Listed before, now deleted. */
  deleted: string[];
  /** The enabled or disabled state differs from the previous release. */
  toggled: string[];
  /** Became first-install only. */
  onceOn: string[];
  /** Stopped being first-install only. */
  onceOff: string[];
  rulesAdded: PublishRule[];
  rulesRemoved: PublishRule[];
  rulesChanged: PublishRule[];
  /** Files the publisher's own instance will remove after following. */
  localDeleted: string[];
}

const ruleKey = (rule: PublishRule): string =>
  `${rule.kind}:${keyOf(rule.path)}`;

/**
 * What this publish changes against the previous release, computed from the
 * payload the dialog would send (`toPublishPayload`) so the two cannot differ.
 * `changes` only tells whether a file's content differs.
 */
export const summarizePublish = (
  context: PublishContext,
  draft: PublishDraft,
  previous: PreviousRelease | null,
  changes: PublishChange[]
): PublishSummary => {
  const payload = toPublishPayload(context, draft);
  const view = viewOf(context, draft);
  const summary: PublishSummary = {
    isNew: previous === null,
    pushed: payload.files.length,
    added: [],
    modified: [],
    onceUpdated: [],
    removed: [],
    ignored: [],
    deleted: [],
    toggled: [],
    onceOn: [],
    onceOff: [],
    rulesAdded: [],
    rulesRemoved: [],
    rulesChanged: [],
    localDeleted: localDeletePaths(context, draft),
  };
  const before = new Map(
    (previous?.files ?? []).map((file) => [keyOf(file.path), file])
  );
  const differs = new Set(
    changes
      .filter(
        (change) => change.kind === "modified" || change.kind === "onceChanged"
      )
      .map((change) => keyOf(change.path))
  );
  const pushed = new Set<string>();
  for (const file of payload.files) {
    const key = keyOf(file.path);
    pushed.add(key);
    const old = before.get(key);
    if (!old) {
      summary.added.push(file.path);
      continue;
    }
    if (differs.has(key)) {
      (file.once ? summary.onceUpdated : summary.modified).push(file.path);
    }
    if (file.disabled !== old.disabled) summary.toggled.push(file.path);
    if (file.once && !old.once) summary.onceOn.push(file.path);
    if (!file.once && old.once) summary.onceOff.push(file.path);
  }
  for (const [key, old] of before) {
    if (pushed.has(key)) continue;
    const state = view.fileState(old.path);
    if (state === "ignore") summary.ignored.push(old.path);
    else if (state === "delete") summary.deleted.push(old.path);
    else summary.removed.push(old.path);
  }
  const oldRules = new Map(
    (previous?.rules ?? []).map((rule) => [ruleKey(rule), rule])
  );
  const newRules = new Map(draft.rules.map((rule) => [ruleKey(rule), rule]));
  for (const [key, rule] of newRules) {
    const old = oldRules.get(key);
    if (!old) summary.rulesAdded.push(rule);
    else if (old.state !== rule.state) summary.rulesChanged.push(rule);
  }
  for (const [key, rule] of oldRules) {
    if (!newRules.has(key)) summary.rulesRemoved.push(rule);
  }
  return summary;
};

/** Whether the summary has nothing to show. */
export const isSummaryEmpty = (summary: PublishSummary): boolean =>
  summary.added.length === 0 &&
  summary.modified.length === 0 &&
  summary.onceUpdated.length === 0 &&
  summary.removed.length === 0 &&
  summary.ignored.length === 0 &&
  summary.deleted.length === 0 &&
  summary.toggled.length === 0 &&
  summary.onceOn.length === 0 &&
  summary.onceOff.length === 0 &&
  summary.rulesAdded.length === 0 &&
  summary.rulesRemoved.length === 0 &&
  summary.rulesChanged.length === 0 &&
  summary.localDeleted.length === 0;
