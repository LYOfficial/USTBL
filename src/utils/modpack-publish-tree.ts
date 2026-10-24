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

export interface PreviousRelease {
  files: PublishFileEntry[];
  rules: PublishRule[];
}

/** A suggested-blacklist entry at the level the blacklist names it. */
export interface SuggestedEntry {
  path: string;
  kind: "file" | "dir";
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

export const DISABLED_SUFFIX = ".disabled";
export const MODS_PREFIX = "mods/";
export const MAX_RULES = 2000;

export const keyOf = (path: string): string => path.toLowerCase();

const isUnder = (key: string, dirKey: string): boolean =>
  key.startsWith(`${dirKey}/`);

/**
 * Turn the listed files into enabled names. `mods/x.jar.disabled` becomes
 * `mods/x.jar` flagged disabled; a `.disabled` file elsewhere cannot be
 * expressed in a manifest and is left out.
 */
export const normalizeDiskFiles = (all: string[]): DiskFile[] => {
  const byKey = new Map<string, DiskFile>();
  const order: string[] = [];
  const put = (path: string, disabled: boolean) => {
    const key = keyOf(path);
    const existing = byKey.get(key);
    if (existing) {
      existing.conflict = true;
      if (!disabled) {
        existing.path = path;
        existing.disabled = false;
      }
      return;
    }
    byKey.set(key, { path, disabled, conflict: false });
    order.push(key);
  };
  for (const path of all) {
    if (keyOf(path).endsWith(DISABLED_SUFFIX)) {
      const base = path.slice(0, path.length - DISABLED_SUFFIX.length);
      if (base !== "" && keyOf(base).startsWith(MODS_PREFIX)) put(base, true);
    } else {
      put(path, false);
    }
  }
  return order.map((key) => byKey.get(key)!);
};

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

const dirKeysOf = (keys: Iterable<string>): Set<string> => {
  const dirs = new Set<string>();
  for (const key of keys) {
    let cut = key.lastIndexOf("/");
    while (cut > 0 && !dirs.has(key.slice(0, cut))) {
      dirs.add(key.slice(0, cut));
      cut = key.lastIndexOf("/", cut - 1);
    }
  }
  return dirs;
};

const stripGhosts = (
  ghosts: GhostEntry[],
  diskKeys: Set<string>,
  diskDirs: Set<string>
): GhostEntry[] => {
  const taken = new Set<string>();
  const fileGhosts = new Set(
    ghosts.filter((g) => g.kind === "file").map((g) => keyOf(g.path))
  );
  return ghosts.filter((ghost) => {
    const key = keyOf(ghost.path);
    if (taken.has(key) || diskKeys.has(key)) return false;
    if (ghost.kind === "file" && diskDirs.has(key)) return false;
    let cut = key.lastIndexOf("/");
    while (cut > 0) {
      const parent = key.slice(0, cut);
      if (diskKeys.has(parent) || fileGhosts.has(parent)) return false;
      cut = key.lastIndexOf("/", cut - 1);
    }
    taken.add(key);
    return true;
  });
};

export interface InitialModelInput {
  /** Every listed file of the instance, `.disabled` names included. */
  all: string[];
  previous: PreviousRelease | null;
  suggested: SuggestedEntry[];
}

/**
 * The tree the dialog opens with (§4 defaults): the previous release's rules
 * carry over as they are, files it listed stay ticked with their `once`, new
 * files follow the nearest rule or are ticked, and a suggested-blacklist entry
 * the previous release never mentioned gets one ignore rule at its own level.
 */
export const createInitialModel = ({
  all,
  previous,
  suggested,
}: InitialModelInput): { context: PublishContext; draft: PublishDraft } => {
  const files = normalizeDiskFiles(all);
  const diskKeys = new Set(files.map((file) => keyOf(file.path)));
  const diskDirs = dirKeysOf(diskKeys);
  const prevFiles = previous?.files ?? [];
  const rules: PublishRule[] = (previous?.rules ?? []).map((rule) => ({
    ...rule,
  }));

  const ghostCandidates: GhostEntry[] = [];
  for (const entry of prevFiles) {
    if (!diskKeys.has(keyOf(entry.path))) {
      ghostCandidates.push({ path: entry.path, kind: "file" });
    }
  }
  for (const rule of rules) {
    const key = keyOf(rule.path);
    const present =
      rule.kind === "file" ? diskKeys.has(key) : diskDirs.has(key);
    if (!present) ghostCandidates.push({ path: rule.path, kind: rule.kind });
  }
  const ghosts = stripGhosts(ghostCandidates, diskKeys, diskDirs);

  const prevByKey = new Map(prevFiles.map((f) => [keyOf(f.path), f]));
  const mentioned = (entry: SuggestedEntry): boolean => {
    const key = keyOf(entry.path);
    const inside = (other: string) => other === key || isUnder(other, key);
    return (
      prevFiles.some((f) => inside(keyOf(f.path))) ||
      rules.some((rule) => {
        const ruleKey = keyOf(rule.path);
        return (
          inside(ruleKey) || (rule.kind === "dir" && isUnder(key, ruleKey))
        );
      })
    );
  };
  for (const entry of suggested) {
    if (!mentioned(entry)) {
      rules.push({ path: entry.path, kind: entry.kind, state: "ignore" });
    }
  }

  const index = indexRules(rules);
  const included = new Set<string>();
  const once = new Set<string>();
  for (const file of files) {
    const key = keyOf(file.path);
    const prev = prevByKey.get(key);
    if (!prev) continue;
    if (prev.once) once.add(key);
    if (!index.files.has(key) && coveringRule(index, key)) included.add(key);
  }

  return { context: { files, ghosts }, draft: { rules, included, once } };
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
 * A directory and everything listed under it takes `state`. Ignore and delete
 * leave one rule on the directory (none if it inherits that already) and drop
 * the settings below; include ticks every file on disk below it.
 */
export const setDirState = (
  context: PublishContext,
  draft: PublishDraft,
  path: string,
  state: PublishState
): PublishDraft => {
  const dirKey = keyOf(path);
  let rules = withoutRules(
    draft.rules,
    (key) => key === dirKey || isUnder(key, dirKey)
  );
  const included = new Set(
    Array.from(draft.included).filter((key) => !isUnder(key, dirKey))
  );
  const covering = coveringRule(indexRules(rules), dirKey);
  const inherited: ShownState = covering ? covering.state : "include";
  if (state === "include") {
    if (inherited !== "include") {
      for (const file of context.files) {
        const key = keyOf(file.path);
        if (isUnder(key, dirKey)) included.add(key);
      }
    }
  } else if (inherited !== state) {
    rules = withRule(rules, { path, kind: "dir", state });
  }
  return { rules, included, once: draft.once };
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

/** The state a click moves to; a mixed or removed node goes to the first. */
export const nextState = (
  current: ShownState,
  options: { canRemove: boolean }
): PublishState | "remove" => {
  const order: (PublishState | "remove")[] = options.canRemove
    ? ["remove", "ignore", "delete"]
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
