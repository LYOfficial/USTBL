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
