export interface SharedInstance {
  id: number;
  name: string;
  createdAt: string;
  updatedAt: string;
}

export interface SharedMod {
  id: number;
  folderId: number | null;
  fileName: string;
  fileSize: number;
  status: "used" | "deleted";
  createdByUsername?: string;
  createdAt: string;
  deletedByUsername?: string;
  deletedAt?: string;
}

export interface SharedFolder {
  id: number;
  parentId: number | null;
  name: string;
  createdAt: string;
  updatedAt: string;
}

export interface SharedInstanceDetail extends SharedInstance {
  folders: SharedFolder[];
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
