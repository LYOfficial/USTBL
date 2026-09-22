//! MultiMC component metadata -> USTBL's existing client JSON / download pipeline.
//! Format/behaviour references (independent Rust implementation): PrismLauncher
//! a94a081b9c1347d65157ae3162d9adb8fee93c6a for component metadata resolution;
//! HMCL f532df20e6ab82b508da4e0b94448a36c118f949 for MultiMC import and
//! 76d35af5b92201d6db827c5a01d9e7caac5ddcaa for `libraries`/`+libraries` merging.
//! No loader-name or mod-name heuristics belong in the resolver.

use super::{ModpackManifest, MultiMcManifest};
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::client_json::*;
use crate::launch::helpers::file_validator::{convert_library_name_to_path, merge_library_lists};
use crate::utils::web::fetch_json_with_fallbacks;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path};
use tauri_plugin_http::reqwest;
use url::Url;
use zip::ZipArchive;

const META_BASE: &str = "https://meta.prismlauncher.org/v1/";
const MAX_COMPONENTS: usize = 128;

fn invalid(message: impl std::fmt::Display) -> USTBLError {
  USTBLError(format!("MultiMC: {message}"))
}

/// Reject ambiguous Windows paths on every host, not only when running on Windows.
pub fn safe_relative(path: &str) -> USTBLResult<&Path> {
  if path.is_empty()
    || path.contains(['\\', ':', '\0'])
    || path.starts_with('/')
    || path.split('/').any(|s| {
      s.is_empty()
        || s == ".."
        || s == "."
        || s.ends_with([' ', '.'])
        || matches!(
          s.split('.')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase()
            .as_str(),
          "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
        )
    })
    || Path::new(path)
      .components()
      .any(|c| !matches!(c, Component::Normal(_)))
  {
    return Err(invalid(format!("unsafe relative path: {path}")));
  }
  Ok(Path::new(path))
}

fn identifier(value: &str) -> USTBLResult<()> {
  if value.is_empty()
    || !value
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
    || value == "."
    || value == ".."
  {
    return Err(invalid(format!("invalid component identifier: {value}")));
  }
  Ok(())
}

fn read_entry(archive: &mut ZipArchive<&File>, name: &str, limit: u64) -> USTBLResult<Vec<u8>> {
  safe_relative(name)?;
  let mut entry = archive.by_name(name)?;
  if entry.is_dir()
    || entry.size() > limit
    || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
  {
    return Err(invalid(format!("invalid archive entry: {name}")));
  }
  let mut bytes = Vec::new();
  (&mut entry).take(limit + 1).read_to_end(&mut bytes)?;
  if bytes.len() as u64 > limit {
    return Err(invalid("archive entry exceeds size limit"));
  }
  Ok(bytes)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Requirement {
  uid: String,
  equals: Option<String>,
  suggests: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Patch {
  uid: String,
  version: String,
  #[serde(default = "one")]
  format_version: u32,
  order: Option<i32>,
  #[serde(default)]
  requires: Vec<Requirement>,
  #[serde(default)]
  conflicts: Vec<Requirement>,
  main_class: Option<String>,
  #[serde(rename = "type")]
  release_type: Option<String>,
  main_jar: Option<Value>,
  minecraft_arguments: Option<String>,
  arguments: Option<LaunchArgumentTemplate>,
  asset_index: Option<AssetIndexInfo>,
  compatible_java_majors: Option<Vec<i32>>,
  compatible_java_name: Option<String>,
  #[serde(default, rename = "+jvmArgs")]
  jvm_args: Vec<String>,
  #[serde(default, rename = "+tweakers")]
  tweakers: Vec<String>,
  #[serde(default, rename = "+traits")]
  traits: Vec<String>,
  #[serde(default)]
  libraries: Vec<Value>,
  #[serde(default, rename = "+libraries")]
  extra_libraries: Vec<Value>,
  #[serde(default)]
  maven_files: Vec<Value>,
  #[serde(flatten)]
  other: HashMap<String, Value>,
}
fn one() -> u32 {
  1
}

impl Patch {
  fn validate(&self) -> USTBLResult<()> {
    identifier(&self.uid)?;
    identifier(&self.version)?;
    if self.format_version != 1 {
      return Err(invalid("unsupported patch formatVersion"));
    }
    // Fail closed for launch-affecting features we cannot yet represent. Never
    // silently downgrade a custom profile to vanilla/stock Forge.
    for key in [
      "jarMods",
      "+jarMods",
      "mods",
      "agents",
      "appletClass",
      "-libraries",
      "-tweakers",
      "-minecraftArguments",
      "+minecraftArguments",
    ] {
      if self.other.get(key).is_some_and(|v| {
        !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()) && v.as_str() != Some("")
      }) {
        return Err(invalid(format!(
          "{} uses unsupported patch field {key}",
          self.uid
        )));
      }
    }
    for trait_ in &self.traits {
      if ![
        "FirstThreadOnMacOS",
        "texturepacks",
        "no-texturepacks",
        "XR:Initial",
        "feature:is_quick_play_singleplayer",
        "feature:is_quick_play_multiplayer",
      ]
      .contains(&trait_.as_str())
      {
        return Err(invalid(format!(
          "{} uses unsupported trait {trait_}",
          self.uid
        )));
      }
    }
    if self
      .compatible_java_majors
      .as_ref()
      .is_some_and(|v| v.is_empty() || v.iter().any(|j| *j < 1))
    {
      return Err(invalid("empty or invalid compatibleJavaMajors"));
    }
    Ok(())
  }
}

pub struct ResolvedMultiMc {
  pub client: McClientInfo,
  patches: Vec<Patch>,
  base: String,
  cfg: HashMap<String, String>,
}

impl ResolvedMultiMc {
  /// Returns None only when the archive is not MultiMC. Malformed MultiMC input
  /// is an error, not permission to drop its custom components.
  pub async fn from_archive(client: &reqwest::Client, file: &File) -> USTBLResult<Option<Self>> {
    let mut archive = ZipArchive::new(file)?;
    if !archive
      .file_names()
      .any(|n| n == "mmc-pack.json" || n.ends_with("/mmc-pack.json"))
    {
      return Ok(None);
    }
    // Keep the same format precedence as the shared importer. A file named
    // mmc-pack.json inside another format's overrides is not a new pack root.
    if crate::instance::helpers::modpack::curseforge::CurseForgeManifest::from_archive(file).is_ok()
      || crate::instance::helpers::modpack::modrinth::ModrinthManifest::from_archive(file).is_ok()
    {
      return Ok(None);
    }
    let manifest = MultiMcManifest::from_archive(file)?;
    if manifest.format_version != 1 || manifest.components.is_empty() {
      return Err(invalid("unsupported or empty manifest"));
    }
    let mut pending = Vec::new();
    let mut patches: Vec<Patch> = Vec::new();
    let mut seen = HashSet::new();
    for c in &manifest.components {
      if !seen.insert(c.uid.clone()) {
        return Err(invalid(format!("duplicate component {}", c.uid)));
      }
      pending.push((
        c.uid.clone(),
        c.version.clone().or(c.cached_version.clone()),
      ));
    }
    let mut index = 0;
    while index < pending.len() {
      if pending.len() > MAX_COMPONENTS {
        return Err(invalid("too many components"));
      }
      let (uid, requested) = &pending[index];
      identifier(uid)?;
      let local = format!("{}patches/{uid}.json", manifest.base_path);
      let patch: Patch = if archive.index_for_name(&local).is_some() {
        serde_json::from_slice(&read_entry(&mut archive, &local, 4 * 1024 * 1024)?)?
      } else {
        let version = requested
          .as_ref()
          .ok_or_else(|| invalid(format!("{uid} has no local patch or pinned version")))?;
        identifier(version)?;
        let url = Url::parse(&format!("{META_BASE}{uid}/{version}.json"))?;
        fetch_json_with_fallbacks(client, &[url], 3).await?
      };
      patch.validate()?;
      if patch.uid != *uid || requested.as_ref().is_some_and(|v| *v != patch.version) {
        return Err(invalid(format!(
          "component identity/version mismatch for {uid}"
        )));
      }
      for dep in &patch.requires {
        identifier(&dep.uid)?;
        if seen.insert(dep.uid.clone()) {
          // Fabric/Quilt mapping components use the Minecraft version as their
          // version namespace. This is format-provider policy, not a pack fix.
          let version = dep.equals.clone().or(dep.suggests.clone()).or_else(|| {
            if matches!(
              dep.uid.as_str(),
              "net.fabricmc.intermediary" | "org.quiltmc.hashed"
            ) {
              pending
                .iter()
                .find(|(uid, _)| uid == "net.minecraft")
                .and_then(|(_, version)| version.clone())
            } else {
              None
            }
          });
          pending.push((dep.uid.clone(), version));
        }
      }
      patches.push(patch);
      index += 1;
    }
    let patches = order_patches(patches)?;
    let mut result = Self {
      client: McClientInfo::default(),
      patches,
      base: manifest.base_path,
      cfg: manifest.cfg,
    };
    result.client = result.compose()?;
    Ok(Some(result))
  }

  fn compose(&self) -> USTBLResult<McClientInfo> {
    let mut info = McClientInfo::default();
    let mut profile = ComponentLaunchProfile {
      platform: ComponentLaunchProfile::current_platform(),
      ..Default::default()
    };
    let mut args = LaunchArgumentTemplate::default();
    let mut tweakers = Vec::new();
    let mut traits = HashSet::new();
    for patch in &self.patches {
      if patch.uid == "net.minecraft" {
        info.client_version = Some(patch.version.clone());
        if let Some(index) = &patch.asset_index {
          info.asset_index = index.clone();
          info.assets = index.id.clone();
        }
      }
      if let Some(main) = &patch.main_class {
        info.main_class = Some(main.clone());
      }
      if let Some(type_) = &patch.release_type {
        info.type_ = type_.clone();
      }
      traits.extend(patch.traits.iter().map(String::as_str));
      if let Some(game) = &patch.minecraft_arguments {
        args.game = split_args(game)?.into_iter().map(arg).collect();
      }
      if let Some(arguments) = &patch.arguments {
        args.game.extend(arguments.game.clone());
        args.jvm.extend(arguments.jvm.clone());
      }
      args.jvm.extend(patch.jvm_args.iter().cloned().map(arg));
      if cfg!(target_os = "macos") && patch.traits.iter().any(|t| t == "FirstThreadOnMacOS") {
        args.jvm.push(arg("-XstartOnFirstThread".into()));
      }
      if let Some(majors) = &patch.compatible_java_majors {
        profile.compatible_java_majors = majors.clone();
      }
      if let Some(name) = &patch.compatible_java_name {
        profile.compatible_java_name = Some(name.clone());
      }
      for tweak in &patch.tweakers {
        tweakers.retain(|t| t != tweak);
        tweakers.push(tweak.clone());
      }
    }
    if info.client_version.is_none() || info.main_class.is_none() || info.asset_index.url.is_empty()
    {
      return Err(invalid(
        "missing Minecraft component, mainClass or assetIndex",
      ));
    }
    for tweak in tweakers {
      args.game.extend([arg("--tweakClass".into()), arg(tweak)]);
    }
    // Prism metadata represents these Mojang feature arguments as traits.
    for (trait_, option, placeholder, features) in [
      (
        "feature:is_quick_play_singleplayer",
        "--quickPlaySingleplayer",
        "${quickPlaySingleplayer}",
        FeaturesInfo {
          is_quick_play_singleplayer: Some(true),
          ..Default::default()
        },
      ),
      (
        "feature:is_quick_play_multiplayer",
        "--quickPlayMultiplayer",
        "${quickPlayMultiplayer}",
        FeaturesInfo {
          is_quick_play_multiplayer: Some(true),
          ..Default::default()
        },
      ),
    ] {
      if traits.contains(trait_)
        && !args
          .game
          .iter()
          .any(|a| a.value.iter().any(|v| v == option))
      {
        args.game.push(ArgumentsItem {
          value: vec![option.into(), placeholder.into()],
          rules: vec![InstructionRule {
            action: "allow".into(),
            features: Some(features),
            ..Default::default()
          }],
        });
      }
    }
    // Required defaults even for legacy Minecraft using modern component JVM args.
    args.jvm.extend([
      arg("-Djava.library.path=${natives_directory}".into()),
      arg("-Dminecraft.launcher.brand=${launcher_name}".into()),
      arg("-Dminecraft.launcher.version=${launcher_version}".into()),
    ]);
    if self
      .cfg
      .get("OverrideJavaArgs")
      .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
      if let Some(value) = self.cfg.get("JvmArgs") {
        args.jvm.extend(split_args(value)?.into_iter().map(arg));
      }
    }
    // Do not execute arbitrary commands shipped in an imported archive.
    if self
      .cfg
      .get("OverrideCommands")
      .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
      for field in ["PreLaunchCommand", "PostExitCommand", "WrapperCommand"] {
        if self.cfg.get(field).is_some_and(|v| !v.trim().is_empty()) {
          return Err(invalid(format!(
            "imported {field} requires manual configuration"
          )));
        }
      }
    }
    info.arguments = Some(args);
    info.component_profile = Some(profile);
    Ok(info)
  }

  /// Materialize local artifacts once, then reuse USTBL's normal validator and
  /// resumable downloader. Never add Maven-only files to the classpath.
  pub fn materialize(&mut self, file: &File, libraries: &Path, instance: &Path) -> USTBLResult<()> {
    let mut archive = ZipArchive::new(file)?;
    let mut normal = Vec::new();
    let mut maven = Vec::new();
    let mut main = None;
    for patch in &self.patches {
      for raw in patch.libraries.iter().chain(&patch.extra_libraries) {
        if let Some(lib) = normalize_library(raw, &mut archive, &self.base, libraries)? {
          normal.push(lib);
        }
      }
      for raw in &patch.maven_files {
        if let Some(lib) = normalize_library(raw, &mut archive, &self.base, libraries)? {
          if let Some(artifact) = lib.downloads.and_then(|d| d.artifact) {
            maven.push(artifact);
          }
        }
      }
      if let Some(raw) = &patch.main_jar {
        main = Some(raw.clone());
      }
    }
    self.client.libraries = merge_library_lists(&normal, &[]);
    let raw = main.ok_or_else(|| invalid("missing mainJar"))?;
    let main = normalize_library(&raw, &mut archive, &self.base, libraries)?
      .and_then(|l| l.downloads)
      .and_then(|d| d.artifact)
      .ok_or_else(|| invalid("missing mainJar artifact"))?;
    if main.url.is_empty() {
      fs::create_dir_all(instance)?;
      fs::copy(
        libraries.join(&main.path),
        instance.join(format!("{}.jar", self.client.id)),
      )?;
    }
    self.client.downloads.insert(
      "client".into(),
      DownloadsValue {
        url: main.url,
        sha1: main.sha1,
        size: main.size,
      },
    );
    self.client.component_profile.as_mut().unwrap().maven_files = maven;
    add_wrapper_arguments(&mut self.client)?;
    fs::create_dir_all(instance)?;
    fs::write(
      instance.join("ustbl-multimc-components.json"),
      serde_json::to_vec_pretty(&self.patches)?,
    )?;
    Ok(())
  }
}

fn arg(value: String) -> ArgumentsItem {
  ArgumentsItem {
    value: vec![value],
    rules: Vec::new(),
  }
}
fn split_args(value: &str) -> USTBLResult<Vec<String>> {
  shlex::split(value).ok_or_else(|| invalid("malformed quoted arguments"))
}

fn order_patches(mut patches: Vec<Patch>) -> USTBLResult<Vec<Patch>> {
  for p in &patches {
    for dep in &p.requires {
      let found = patches
        .iter()
        .find(|p| p.uid == dep.uid)
        .ok_or_else(|| invalid(format!("missing dependency {}", dep.uid)))?;
      if dep.equals.as_ref().is_some_and(|v| *v != found.version) {
        return Err(invalid(format!(
          "{} requires {} = {:?}",
          p.uid, dep.uid, dep.equals
        )));
      }
    }
    for conflict in &p.conflicts {
      if patches
        .iter()
        .any(|p| p.uid == conflict.uid && conflict.equals.as_ref().is_none_or(|v| *v == p.version))
      {
        return Err(invalid(format!(
          "{} conflicts with {}",
          p.uid, conflict.uid
        )));
      }
    }
  }
  // Explicit order is a legacy hint. Dependencies must always precede consumers;
  // stable sorting keeps manifest order for patches without the hint.
  patches.sort_by_key(|p| p.order.unwrap_or(0));
  let mut result = Vec::new();
  let mut done = HashSet::new();
  while !patches.is_empty() {
    let index = patches
      .iter()
      .position(|p| p.requires.iter().all(|r| done.contains(&r.uid)))
      .ok_or_else(|| invalid("cyclic component dependencies"))?;
    let p = patches.remove(index);
    done.insert(p.uid.clone());
    result.push(p);
  }
  Ok(result)
}

fn host_os() -> &'static str {
  if cfg!(windows) {
    "windows"
  } else if cfg!(target_os = "macos") {
    "osx"
  } else {
    "linux"
  }
}

/// MultiMC has platform names such as linux-arm64 in addition to Mojang's os.arch.
fn rule_matches(rule: &InstructionRule) -> USTBLResult<bool> {
  if rule.features.is_some() {
    return Err(invalid(
      "feature rules on component libraries are unsupported",
    ));
  }
  let Some(os) = &rule.os else {
    return Ok(true);
  };
  let suffix = match std::env::consts::ARCH {
    "aarch64" => "arm64",
    "arm" => "arm32",
    "x86" => "x86",
    _ => "",
  };
  let platform = format!("{}-{suffix}", host_os());
  if !os.name.is_empty()
    && os.name != host_os()
    && os.name != platform
    && !(os.name == "macos" && host_os() == "osx")
  {
    return Ok(false);
  }
  if let Some(arch) = &os.arch {
    let matches = match arch.as_str() {
      "amd64" | "x86_64" => cfg!(target_arch = "x86_64"),
      "x86" => cfg!(target_arch = "x86"),
      "aarch64" | "arm64" => cfg!(target_arch = "aarch64"),
      _ => arch == std::env::consts::ARCH,
    };
    if !matches {
      return Ok(false);
    }
  }
  if let Some(version) = &os.version {
    return Ok(regex::Regex::new(version)?.is_match(&tauri_plugin_os::version().to_string()));
  }
  Ok(true)
}

fn normalize_library(
  raw: &Value,
  archive: &mut ZipArchive<&File>,
  base: &str,
  libraries: &Path,
) -> USTBLResult<Option<LibrariesValue>> {
  let mut lib: LibrariesValue = serde_json::from_value(raw.clone())?;
  if lib.name.is_empty() {
    return Err(invalid("missing library coordinate"));
  }
  let mut allowed = lib.rules.is_empty();
  for rule in &lib.rules {
    if !["allow", "disallow"].contains(&rule.action.as_str()) {
      return Err(invalid("unknown library rule action"));
    }
    if rule_matches(rule)? {
      allowed = rule.action == "allow";
    }
  }
  if !allowed {
    return Ok(None);
  }
  // This imported instance is resolved for the current platform. Keep originals
  // in the component snapshot, and never re-evaluate with Mojang-only rules.
  lib.rules.clear();
  let rel = convert_library_name_to_path(&lib.name, None)?;
  safe_relative(&rel)?;
  if raw.get("MMC-hint").and_then(Value::as_str) == Some("local") {
    if lib.natives.is_some() {
      return Err(invalid("local native classifiers are unsupported"));
    }
    let filename = raw
      .get("MMC-filename")
      .and_then(Value::as_str)
      .unwrap_or(rel.rsplit('/').next().unwrap());
    safe_relative(filename)?;
    let bytes = read_entry(
      archive,
      &format!("{base}libraries/{filename}"),
      256 * 1024 * 1024,
    )?;
    let sha1 = hex::encode(Sha1::digest(&bytes));
    let path = format!("ustbl-local/{sha1}/{}", rel.rsplit('/').next().unwrap());
    let target = libraries.join(&path);
    fs::create_dir_all(target.parent().unwrap())?;
    fs::write(&target, &bytes)?;
    lib.downloads = Some(LibrariesDownloads {
      artifact: Some(DownloadsArtifact {
        path,
        url: String::new(),
        sha1,
        size: bytes.len() as i64,
      }),
      classifiers: None,
    });
    return Ok(Some(lib));
  }
  let repository = raw
    .get("url")
    .and_then(Value::as_str)
    .unwrap_or("https://libraries.minecraft.net/");
  let downloads = lib.downloads.get_or_insert_with(Default::default);
  if lib.natives.is_none() {
    let artifact = downloads.artifact.get_or_insert_with(Default::default);
    if let Some(url) = raw
      .get("MMC-absoluteUrl")
      .or_else(|| raw.get("MMC-absulute_url"))
      .and_then(Value::as_str)
    {
      artifact.url = url.into();
    }
    normalize_artifact(artifact, &rel, repository)?;
  } else if let Some(natives) = &mut lib.natives {
    if raw
      .get("MMC-absoluteUrl")
      .or_else(|| raw.get("MMC-absulute_url"))
      .is_some()
    {
      return Err(invalid(
        "absolute URL overrides on native classifiers are unsupported",
      ));
    }
    let suffix = match std::env::consts::ARCH {
      "aarch64" => "arm64",
      "arm" => "arm32",
      _ => "",
    };
    let platform = format!("{}-{suffix}", host_os());
    if let Some(classifier) = natives.get(&platform).cloned() {
      natives.insert(host_os().into(), classifier);
    }
    let Some(native) = crate::launch::helpers::misc::get_natives_string(natives) else {
      return Ok(None);
    };
    let artifact = downloads
      .classifiers
      .get_or_insert_with(Default::default)
      .entry(native.clone())
      .or_default();
    normalize_artifact(
      artifact,
      &convert_library_name_to_path(&lib.name, Some(native))?,
      repository,
    )?;
  }
  Ok(Some(lib))
}

fn normalize_artifact(
  artifact: &mut DownloadsArtifact,
  default_path: &str,
  repository: &str,
) -> USTBLResult<()> {
  if artifact.path.is_empty() {
    artifact.path = default_path.into();
  }
  safe_relative(&artifact.path)?;
  if artifact.url.is_empty() {
    artifact.url = format!("{}/{}", repository.trim_end_matches('/'), default_path);
  }
  let url = Url::parse(&artifact.url)?;
  if !["http", "https"].contains(&url.scheme())
    || !url.username().is_empty()
    || url.password().is_some()
  {
    return Err(invalid("invalid artifact URL"));
  }
  Ok(())
}

fn add_wrapper_arguments(info: &mut McClientInfo) -> USTBLResult<()> {
  if info
    .libraries
    .iter()
    .any(|l| l.name.starts_with("io.github.zekerzhayard:ForgeWrapper:"))
  {
    let profile = info.component_profile.as_ref().unwrap();
    let installer = profile
      .maven_files
      .iter()
      .chain(
        info
          .libraries
          .iter()
          .filter_map(|l| l.downloads.as_ref()?.artifact.as_ref()),
      )
      .find(|a| {
        a.path.ends_with("-installer.jar")
          && (a.path.starts_with("net/minecraftforge/") || a.path.starts_with("net/neoforged/"))
      })
      .ok_or_else(|| invalid("ForgeWrapper installer is missing"))?;
    info.arguments.as_mut().unwrap().jvm.extend([
      arg("-Dforgewrapper.librariesDir=${library_directory}".into()),
      arg("-Dforgewrapper.minecraft=${primary_jar}".into()),
      arg(format!(
        "-Dforgewrapper.installer=${{library_directory}}/{}",
        installer.path
      )),
    ]);
  }
  Ok(())
}

#[cfg(test)]
mod tests;
