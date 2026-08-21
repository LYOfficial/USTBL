export interface VustbProfile {
  id: string;
  name: string;
  selected: boolean;
}

export interface VustbAccount {
  subject: string;
  username: string;
  avatarUrl: string;
  userGroup: string;
  profiles: VustbProfile[];
  playerId: string;
}
