export interface VustbProfile {
  id: string;
  name: string;
  selected: boolean;
}

export interface VustbProgression {
  experience: number;
  level: number;
  maxLevel: number;
  maxExperience: number;
  levelStartExperience: number;
  nextLevelExperience: number;
  experienceIntoLevel: number;
  experienceForNextLevel: number;
  isMaxLevel: boolean;
  checkinDays: number;
  playTimeSeconds: number;
}

export interface VustbAccount {
  subject: string;
  username: string;
  avatarUrl: string;
  userGroup: string;
  profiles: VustbProfile[];
  progression: VustbProgression;
  lastCheckin: string | null;
  playerId: string;
}

export interface VustbCheckinResult {
  message: string;
  experienceGained: number;
  account: VustbAccount;
}

export interface VustbFriend {
  friendshipId: number;
  id: number;
  username: string;
  displayName: string;
  avatarUrl: string;
  online: boolean;
  instanceName: string | null;
  lastSeenAt: string | null;
}
