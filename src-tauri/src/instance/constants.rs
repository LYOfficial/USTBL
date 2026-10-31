pub const INSTANCE_CFG_FILE_NAME: &str = "ustblcfg.json";
#[allow(dead_code)]
pub const LEGACY_INSTANCE_CFG_FILE_NAME: &str = "sjmclcfg.json";
pub const TRANSLATION_CACHE_EXPIRY_HOURS: u64 = 24;
pub const TRANSLATION_CACHE_FILE_NAME: &str = "local_mod_translations.json";
/// Entries untouched for this long are dropped when the cache is saved.
pub const TRANSLATION_CACHE_RETENTION_HOURS: u64 = 30 * 24;
/// Upper bound on cached mod entries (and, separately, remembered file hashes).
pub const TRANSLATION_CACHE_MAX_ENTRIES: usize = 5000;
pub const COMPRESSED_ICON_SIZE: (u32, u32) = (64, 64);
