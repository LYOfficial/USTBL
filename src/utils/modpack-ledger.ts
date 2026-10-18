import { ModLoaderType } from "@/enums/instance";
import { CampusModpackProject } from "@/models/campus-modpack";
import { InstanceSummary, OriginPlatform } from "@/models/instance/misc";
import {
  ModpackCheckStatus,
  ModpackUpdateCheck,
} from "@/models/modpack-update";

/** The slug the platforms use for a loader type. */
export const loaderSlug = (loader: ModLoaderType): string =>
  loader === ModLoaderType.Unknown ? "vanilla" : loader.toLowerCase();

/** An origin may reference a campus project by slug or by id. */
export const campusProjectRefs = (project: CampusModpackProject): string[] => [
  project.slug,
  String(project.id),
];

export interface LineQuery {
  platform: OriginPlatform;
  projectRefs: string[];
  gameVersion?: string | null;
  loaderType?: string | null;
}

/** The local instances that follow the given line of a project. */
export const findInstancesOfLine = (
  instances: InstanceSummary[],
  query: LineQuery
): InstanceSummary[] =>
  instances.filter(({ origin }) => {
    if (!origin || origin.platform !== query.platform) return false;
    if (!query.projectRefs.includes(origin.projectId)) return false;
    if (query.gameVersion && origin.gameVersion !== query.gameVersion)
      return false;
    if (
      query.loaderType &&
      loaderSlug(origin.loaderType) !== query.loaderType.toLowerCase()
    )
      return false;
    return true;
  });

/**
 * What a ledger row's badge shows: where the instance stands against its
 * line, or not checked yet. Local edits are not a line state; the row shows
 * them on their own (see `ledgerLocalChanges`).
 */
export type LedgerBadge =
  | Exclude<ModpackCheckStatus, "notFollowing" | "localChanges">
  | "notChecked";

export const ledgerBadge = (
  instance: InstanceSummary,
  check: ModpackUpdateCheck | undefined
): LedgerBadge => {
  if (!check || check.status === "notFollowing") return "notChecked";
  if (check.status === "localChanges") return "upToDate";
  return check.status;
};

/** Managed files changed here, once the local scan has run; else 0. */
export const ledgerLocalChanges = (
  check: ModpackUpdateCheck | undefined
): number => check?.localChanges ?? 0;

/**
 * The ledger of modpack instances: only those with an origin. An instance
 * without one carries no identity and is an ordinary instance.
 */
export const ledgerInstances = (
  instances: InstanceSummary[]
): InstanceSummary[] => instances.filter((instance) => instance.origin);
