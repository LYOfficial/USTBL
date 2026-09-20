import { t } from "i18next";
import { PlayerType } from "@/enums/account";
import { Player } from "@/models/account";

export const EXPIRED_ACCESS_TOKEN_PLACEHOLDER = "%failed:access_token_expired%";

export function isVustbPlayer(player: Player) {
  if (player.playerType !== PlayerType.ThirdParty || !player.authServer)
    return false;
  try {
    const url = new URL(player.authServer.authUrl);
    return (
      url.origin === "https://www.ustb.world" &&
      url.pathname.replace(/\/+$/, "") === "/skinapi" &&
      !url.search &&
      !url.hash
    );
  } catch {
    return false;
  }
}

export function isUuidValid(uuid: string) {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(
    uuid
  );
}

export function isOfflinePlayernameValid(name: string) {
  return /^[a-zA-Z0-9_]{1,16}$/.test(name);
}

export const generatePlayerDesc = (player: Player, detailed: boolean) => {
  return player.playerType === PlayerType.Offline ||
    player.playerType === PlayerType.Microsoft
    ? t(`Enums.playerTypes.${player.playerType}`)
    : detailed
      ? `${t("Enums.playerTypes.3rdparty")} - ${player.authServer?.name} (${player.authAccount})`
      : player.authServer?.name || "";
};
