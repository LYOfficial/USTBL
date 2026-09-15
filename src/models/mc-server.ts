export interface McServerMotdSegment {
  text: string;
  color: string | null;
  bold: boolean;
  italic: boolean;
  underlined: boolean;
  strikethrough: boolean;
  obfuscated: boolean;
}

export interface McServerStatus {
  id: number;
  name: string;
  address: string | null;
  description: string | null;
  iconUrl: string | null;
  bannerUrls: string[];
  versionHint: string | null;
  theme: string | null;
  parentId: number | null;
  exposeIp: boolean;
  motdSegments: McServerMotdSegment[];
  connectMs: number | null;
  protocol: number | null;
  playersOnline: number | null;
  playersMax: number | null;
  lastUpdate: string | null;
  serverStatus: string;
  type: string | null;
  version: string | null;
  icon: string | null;
}
