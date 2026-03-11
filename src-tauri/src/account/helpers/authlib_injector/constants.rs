pub static AUTHLIB_INJECTOR_JAR_NAME: &str = "authlib-injector.jar";
pub static USTB_AUTH_SERVER_URL: &str = "https://skin.ustb.world/skinapi";
pub static USTB_OPENID_CONFIGURATION_URL: &str =
  "https://skin.ustb.world/skinapi/.well-known/openid-configuration";
pub static USTB_HOMEPAGE_URL: &str = "https://skin.ustb.world/";
pub static PRESET_AUTH_SERVERS: [&str; 3] = [
  USTB_AUTH_SERVER_URL,
  "https://skin.mualliance.ltd/api/yggdrasil",
  "https://littleskin.cn/api/yggdrasil",
];
pub static SCOPE: &str =
  "openid offline_access Yggdrasil.PlayerProfiles.Select Yggdrasil.Server.Join";

pub static CLIENT_IDS: [(&str, &str); 6] = [
  // built-in preset auth servers
  ("skin.ustb.world", "4"),
  ("skin.mualliance.ltd", "27"),
  ("littleskin.cn", "1014"),
  // supported MUA auth servers (ref: https://github.com/SJMC-Dev/SJMCL-client-ids)
  ("skin.jsumc.fun", "2"),
  ("skin.mc.taru.xj.cn", "6"),
  ("user.suesmc.ltd", "4"),
];
