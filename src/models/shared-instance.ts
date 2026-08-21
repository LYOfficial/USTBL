export interface SharedInstance {
  id: number;
  name: string;
  createdAt: string;
  updatedAt: string;
}

export interface SharedMod {
  id: number;
  fileName: string;
  fileSize: number;
  status: "used" | "deleted";
  createdByUsername?: string;
  createdAt: string;
  deletedByUsername?: string;
  deletedAt?: string;
}

export interface SharedInstanceDetail extends SharedInstance {
  mods: SharedMod[];
}

export interface SharedUpdateResult {
  deleted: string[];
  downloaded: string[];
  skipped: string[];
}

export interface SharedUpdateProgress {
  sharedInstanceId: number;
  current: number;
  total: number;
  fileName?: string;
}
