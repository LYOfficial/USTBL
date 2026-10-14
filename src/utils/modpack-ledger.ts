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

/** What a ledger row shows: an engine status, or not checked yet. */
export type LedgerBadge =
  | Exclude<ModpackCheckStatus, "notFollowing">
  | "notChecked";

export const ledgerBadge = (
  instance: InstanceSummary,
  check: ModpackUpdateCheck | undefined
): LedgerBadge => {
  if (!check || check.status === "notFollowing") return "notChecked";
  return check.status;
};

/**
 * The ledger of modpack instances: only those with an origin. An instance
 * without one carries no identity and is an ordinary instance.
 */
export const ledgerInstances = (
  instances: InstanceSummary[]
): InstanceSummary[] => instances.filter((instance) => instance.origin);
