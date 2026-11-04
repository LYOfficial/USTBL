// How a kind of file change looks, shared by the publish tree, the local
// changes dialog and the update preview.

/** The kinds of change a file can show. */
export type ChangeVisual =
  | "modified"
  | "added"
  | "enable"
  | "disable"
  | "onceChanged"
  | "deleted"
  | "addedExcluded";

export const CHANGE_COLOR: Record<ChangeVisual, string> = {
  modified: "orange",
  added: "green",
  enable: "purple",
  disable: "purple",
  onceChanged: "cyan",
  deleted: "red",
  addedExcluded: "gray",
};

/** The label of a change kind; an uncounted addition reads as an addition. */
export const changeLabelKey = (visual: ChangeVisual): string =>
  `FilePanel.change.${visual === "addedExcluded" ? "added" : visual}`;

/** The color of each group of the update preview. */
export const PLAN_GROUP_COLOR = {
  restored: "orange",
  add: "green",
  update: "blue",
  delete: "red",
  disable: "gray",
  enable: "gray",
  retire: "gray",
  autoMerged: "green",
} as const;
