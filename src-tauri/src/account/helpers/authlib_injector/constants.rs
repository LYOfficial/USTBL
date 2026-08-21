pub static AUTHLIB_INJECTOR_JAR_NAME: &str = "authlib-injector.jar";
pub static USTB_AUTH_SERVER_URL: &str = "https://www.ustb.world/skinapi/";
pub static USTB_OPENID_CONFIGURATION_URL: &str =
  "https://www.ustb.world/.well-known/openid-configuration";
pub static USTB_HOMEPAGE_URL: &str = "https://www.ustb.world/";
// 像素北科是启动器内置的唯一第三方认证服务。其他皮肤站可由用户按需添加。
pub static PRESET_AUTH_SERVERS: [&str; 1] = [USTB_AUTH_SERVER_URL];
pub static SCOPE: &str =
  "openid offline_access userinfo permission Yggdrasil.PlayerProfiles.Select Yggdrasil.Server.Join";

pub static USTB_REDIRECT_URI: &str = "https://www.ustb.world/oauth/device-callback";
pub static USTB_CLIENT_SECRET: &str = "jR4Gno6UPYbftHIgt2hYJSdGg_4MOllRUM4xkePXEoJP8cxn";

pub static CLIENT_IDS: [(&str, &str); 1] = [("www.ustb.world", "1")];
