use super::*;
use std::io::Write;
use std::path::PathBuf;
use zip::write::SimpleFileOptions;

struct Temp(PathBuf);
impl Temp {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ustbl-multimc-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&path).unwrap();
    Self(path)
  }
}
impl Drop for Temp {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn patch(uid: &str, requires: &[&str]) -> Patch {
  serde_json::from_value(serde_json::json!({"uid":uid,"version":"1", "requires":requires.iter().map(|uid|serde_json::json!({"uid":uid,"equals":"1"})).collect::<Vec<_>>() })).unwrap()
}

#[test]
fn dependencies_are_ordered_and_cycles_conflicts_and_versions_fail() {
  let game = patch("net.minecraft", &["org.lwjgl3"]);
  let graphics = patch("org.lwjgl3", &[]);
  let result = order_patches(vec![game.clone(), graphics.clone()]).unwrap();
  assert_eq!(result[0].uid, "org.lwjgl3");
  assert!(order_patches(vec![game.clone()]).is_err());
  assert!(order_patches(vec![game.clone(), patch("org.lwjgl3", &["net.minecraft"])]).is_err());
  let mut other_version = graphics.clone();
  other_version.version = "2".into();
  assert!(order_patches(vec![game.clone(), other_version]).is_err());
  let mut conflict = graphics;
  conflict.conflicts.push(Requirement {
    uid: "net.minecraft".into(),
    equals: None,
    suggests: None,
  });
  assert!(order_patches(vec![game, conflict]).is_err());
}

#[test]
fn rejects_unsafe_paths_and_unsupported_launch_features() {
  for value in [
    "../escape",
    "/absolute",
    "C:/absolute",
    "a\\b",
    "dir/../x",
    "jar:stream",
    "dir./file",
    "./x",
    "a//b",
    "CON.jar",
    "dir/lpt1",
  ] {
    assert!(safe_relative(value).is_err(), "{value}");
  }
  assert!(safe_relative("com/example/library/1.0/library-1.0.jar").is_ok());
  let mut p = patch("custom", &[]);
  p.other.insert(
    "jarMods".into(),
    serde_json::json!([{"name":"unknown:mod:1"}]),
  );
  assert!(p.validate().is_err());
  p.other.clear();
  p.traits.push("unknownLaunchMode".into());
  assert!(p.validate().is_err());
}

fn archive(temp: &Temp, game_dir: &str) -> File {
  let path = temp.0.join("pack.zip");
  let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
  // Synthetic metadata only. No Minecraft, GTNH or third-party jar is distributed.
  let entries = vec![
    ("pack/mmc-pack.json".into(), serde_json::json!({"formatVersion":1,"components":[{"uid":"net.minecraft","version":"1.7.10"},{"uid":"example.bootstrap","version":"1"}]}).to_string()),
    ("pack/instance.cfg".into(), "name=Synthetic Pack\nOverrideJavaArgs=false".into()),
    ("pack/patches/net.minecraft.json".into(), serde_json::json!({"uid":"net.minecraft","version":"1.7.10","compatibleJavaMajors":[17,21],"compatibleJavaName":"java-runtime-test","mainClass":"original.Main","mainJar":{"name":"test:client:1","MMC-hint":"local"},"assetIndex":{"id":"test","url":"https://example.invalid/assets.json"},"minecraftArguments":"--username ${auth_player_name}","libraries":[{"name":"test:dependency:1.0","url":"https://example.invalid/maven/"}]}).to_string()),
    ("pack/patches/example.bootstrap.json".into(), serde_json::json!({"uid":"example.bootstrap","version":"1","mainClass":"example.bootstrap.Main","+jvmArgs":["-Dexample=value"],"requires":[{"uid":"net.minecraft","equals":"1.7.10"}],"libraries":[{"name":"test:bootstrap:1","MMC-hint":"local"},{"name":"test:dependency:2.0","url":"https://example.invalid/maven/"}],"mavenFiles":[{"name":"test:installer:1:installer","url":"https://example.invalid/"}]}).to_string()),
    ("pack/libraries/bootstrap-1.jar".into(), "synthetic early classpath".into()),
    ("pack/libraries/client-1.jar".into(), "synthetic client".into()),
    (format!("pack/{game_dir}/config/example.cfg"), "example=true".into()),
  ];
  for (name, text) in entries {
    zip.start_file(name, SimpleFileOptions::default()).unwrap();
    zip.write_all(text.as_bytes()).unwrap();
  }
  zip.finish().unwrap();
  File::open(path).unwrap()
}

#[tokio::test]
async fn local_components_resolve_offline_and_round_trip_with_ordered_classpath() {
  for game_dir in [".minecraft", "minecraft"] {
    let temp = Temp::new();
    let file = archive(&temp, game_dir);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut resolved = ResolvedMultiMc::from_archive(&client, &file)
      .await
      .unwrap()
      .unwrap();
    resolved.client.id = "imported".into();
    let libraries = temp.0.join("libraries");
    let instance = temp.0.join("imported");
    resolved.materialize(&file, &libraries, &instance).unwrap();
    crate::instance::helpers::modpack::misc::extract_overrides(&file, &instance).unwrap();
    assert_eq!(
      fs::read_to_string(instance.join("config/example.cfg")).unwrap(),
      "example=true"
    );
    assert_eq!(
      resolved.client.main_class.as_deref(),
      Some("example.bootstrap.Main")
    );
    assert_eq!(
      resolved
        .client
        .component_profile
        .as_ref()
        .unwrap()
        .compatible_java_majors,
      [17, 21]
    );
    assert_eq!(resolved.client.libraries.len(), 2);
    assert_eq!(resolved.client.libraries[0].name, "test:dependency:2.0");
    assert_eq!(resolved.client.libraries[1].name, "test:bootstrap:1");
    assert!(instance.join("imported.jar").is_file());
    let round_trip: McClientInfo =
      serde_json::from_slice(&serde_json::to_vec(&resolved.client).unwrap()).unwrap();
    let paths =
      crate::launch::helpers::file_validator::get_nonnative_library_paths(&round_trip, &libraries)
        .unwrap();
    assert!(paths[1].is_file());
    assert!(!paths
      .iter()
      .any(|p| p.to_string_lossy().contains("installer")));
    let tasks = crate::launch::helpers::file_validator::get_invalid_library_files(
      &[],
      &libraries,
      &round_trip,
      true,
    )
    .await
    .unwrap();
    assert_eq!(tasks.len(), 2); // dependency + download-only installer, not bundled jar
    assert!(round_trip
      .arguments
      .unwrap()
      .jvm
      .iter()
      .any(|a| a.value == ["-Dexample=value"]));
  }
}

#[tokio::test]
async fn corrupt_multimc_is_not_silently_treated_as_a_standard_pack() {
  let temp = Temp::new();
  let path = temp.0.join("bad.zip");
  let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
  zip
    .start_file("mmc-pack.json", SimpleFileOptions::default())
    .unwrap();
  zip.write_all(b"not json").unwrap();
  zip.finish().unwrap();
  assert!(
    ResolvedMultiMc::from_archive(&reqwest::Client::new(), &File::open(path).unwrap())
      .await
      .is_err()
  );
}

#[test]
fn last_matching_library_rule_wins() {
  let temp = Temp::new();
  let file = archive(&temp, ".minecraft");
  let mut zip = ZipArchive::new(&file).unwrap();
  let raw = serde_json::json!({"name":"test:excluded:1", "rules":[{"action":"allow"},{"action":"disallow","os":{"name":host_os()}}]});
  assert!(normalize_library(&raw, &mut zip, "pack/", &temp.0)
    .unwrap()
    .is_none());
}

#[tokio::test]
async fn modrinth_overrides_do_not_change_the_detected_pack_format() {
  let temp = Temp::new();
  let path = temp.0.join("modrinth.zip");
  let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
  zip
    .start_file("modrinth.index.json", SimpleFileOptions::default())
    .unwrap();
  zip.write_all(br#"{"versionId":"1","name":"Other format","files":[],"dependencies":{"minecraft":"1.20.1"}}"#).unwrap();
  zip
    .start_file("overrides/mmc-pack.json", SimpleFileOptions::default())
    .unwrap();
  zip.write_all(b"not a pack manifest").unwrap();
  zip.finish().unwrap();
  assert!(
    ResolvedMultiMc::from_archive(&reqwest::Client::new(), &File::open(path).unwrap())
      .await
      .unwrap()
      .is_none()
  );
}

#[tokio::test]
async fn natives_obey_exclusions_and_order_and_propagate_errors() {
  let temp = Temp::new();
  let libraries = temp.0.join("libraries");
  let natives = temp.0.join("natives");
  fs::create_dir_all(&libraries).unwrap();
  let mut info = McClientInfo::default();
  for (name, contents) in [("first", "first"), ("second", "second")] {
    let filename = format!("{name}.jar");
    let mut zip = zip::ZipWriter::new(File::create(libraries.join(&filename)).unwrap());
    for entry in ["native.dll", "META-INF/MANIFEST.MF", "exclude.txt"] {
      zip.start_file(entry, SimpleFileOptions::default()).unwrap();
      zip.write_all(contents.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    info.libraries.push(LibrariesValue {
      name: format!("test:{name}:1"),
      natives: Some(HashMap::from([(host_os().into(), "native".into())])),
      downloads: Some(LibrariesDownloads {
        classifiers: Some(HashMap::from([(
          "native".into(),
          DownloadsArtifact {
            path: filename,
            ..Default::default()
          },
        )])),
        ..Default::default()
      }),
      extract: Some(LibrariesExtract {
        exclude: Some(vec!["exclude.txt".into()]),
      }),
      ..Default::default()
    });
  }
  crate::launch::helpers::file_validator::extract_native_libraries(
    &info, &libraries, &natives, false, false,
  )
  .await
  .unwrap();
  assert_eq!(
    fs::read_to_string(natives.join("native.dll")).unwrap(),
    "second"
  );
  assert!(!natives.join("exclude.txt").exists());
  assert!(!natives.join("META-INF").exists());
  fs::write(libraries.join("second.jar"), b"invalid zip").unwrap();
  assert!(
    crate::launch::helpers::file_validator::extract_native_libraries(
      &info, &libraries, &natives, false, false
    )
    .await
    .is_err()
  );
}

#[test]
fn declared_download_path_and_absolute_url_are_preserved() {
  let temp = Temp::new();
  let file = archive(&temp, ".minecraft");
  let mut zip = ZipArchive::new(&file).unwrap();
  let raw = serde_json::json!({"name":"test:dependency:1", "MMC-absoluteUrl":"https://example.invalid/custom.jar", "downloads":{"artifact":{"path":"custom/dependency.jar"}}});
  let lib = normalize_library(&raw, &mut zip, "pack/", &temp.0)
    .unwrap()
    .unwrap();
  let artifact = lib.downloads.unwrap().artifact.unwrap();
  assert_eq!(artifact.path, "custom/dependency.jar");
  assert_eq!(artifact.url, "https://example.invalid/custom.jar");
}

#[test]
fn forge_wrapper_uses_download_only_installer_and_primary_jar() {
  let mut info = McClientInfo {
    libraries: vec![LibrariesValue {
      name: "io.github.zekerzhayard:ForgeWrapper:1.6.0".into(),
      ..Default::default()
    }],
    arguments: Some(Default::default()),
    component_profile: Some(ComponentLaunchProfile {
      maven_files: vec![DownloadsArtifact {
        path: "net/minecraftforge/forge/1/forge-1-installer.jar".into(),
        ..Default::default()
      }],
      ..Default::default()
    }),
    ..Default::default()
  };
  add_wrapper_arguments(&mut info).unwrap();
  let args = &info.arguments.unwrap().jvm;
  assert!(args
    .iter()
    .any(|a| a.value == ["-Dforgewrapper.minecraft=${primary_jar}"]));
  assert!(args.iter().any(|a| a.value == ["-Dforgewrapper.installer=${library_directory}/net/minecraftforge/forge/1/forge-1-installer.jar"]));
  assert_eq!(info.libraries.len(), 1);
}

#[test]
fn platform_specific_profile_requires_reimport_on_another_platform() {
  let mut profile = ComponentLaunchProfile {
    platform: ComponentLaunchProfile::current_platform(),
    ..Default::default()
  };
  assert!(profile.validate_platform().is_ok());
  profile.platform = "other-platform".into();
  assert!(profile.validate_platform().is_err());
}

/// Opt-in integration check against a user's archive; never redistribute it.
#[tokio::test]
#[ignore = "set USTBL_MULTIMC_TEST_ARCHIVE to a real MultiMC archive"]
async fn real_archive_resolves_to_a_persisted_launch_profile() {
  let temp = Temp::new();
  let file =
    File::open(std::env::var("USTBL_MULTIMC_TEST_ARCHIVE").expect("archive path")).unwrap();
  let mut resolved = ResolvedMultiMc::from_archive(&reqwest::Client::new(), &file)
    .await
    .unwrap()
    .unwrap();
  resolved.client.id = "integration".into();
  resolved
    .materialize(&file, &temp.0.join("libraries"), &temp.0.join("instance"))
    .unwrap();
  assert!(resolved.client.main_class.is_some());
  assert!(resolved.client.downloads.contains_key("client"));
  if let Ok(output) = std::env::var("USTBL_MULTIMC_TEST_OUTPUT") {
    fs::write(output, serde_json::to_vec_pretty(&resolved.client).unwrap()).unwrap();
  }
  println!(
    "entry={:?}, java={:?}, libraries={}",
    resolved.client.main_class,
    resolved.client.component_profile,
    resolved.client.libraries.len()
  );
}

#[tokio::test]
#[ignore = "fetches pinned component metadata from Prism; no game binaries"]
async fn remote_fabric_and_forge_metadata_resolve_without_local_patches() {
  let client = reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(30))
    .build()
    .unwrap();
  for (uid, version, main) in [
    (
      "net.fabricmc.fabric-loader",
      "0.16.14",
      "net.fabricmc.loader.impl.launch.knot.KnotClient",
    ),
    (
      "net.minecraftforge",
      "47.4.0",
      "io.github.zekerzhayard.forgewrapper.installer.Main",
    ),
  ] {
    let temp = Temp::new();
    let path = temp.0.join("remote.zip");
    let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
    zip
      .start_file("mmc-pack.json", SimpleFileOptions::default())
      .unwrap();
    zip.write_all(serde_json::json!({"formatVersion":1,"components":[{"uid":"net.minecraft","version":"1.20.1"},{"uid":uid,"version":version}]}).to_string().as_bytes()).unwrap();
    zip
      .start_file("instance.cfg", SimpleFileOptions::default())
      .unwrap();
    zip.write_all(b"name=Remote test").unwrap();
    zip.finish().unwrap();
    let file = File::open(path).unwrap();
    let mut resolved = ResolvedMultiMc::from_archive(&client, &file)
      .await
      .unwrap()
      .unwrap();
    resolved.client.id = "remote".into();
    resolved
      .materialize(&file, &temp.0.join("libraries"), &temp.0.join("instance"))
      .unwrap();
    assert_eq!(resolved.client.main_class.as_deref(), Some(main));
    assert_eq!(resolved.client.type_, "release");
    let profile = resolved.client.component_profile.as_ref().unwrap();
    assert_eq!(profile.compatible_java_majors, [17]);
    assert!(resolved
      .client
      .arguments
      .as_ref()
      .unwrap()
      .game
      .iter()
      .any(|a| a
        .value
        .first()
        .is_some_and(|v| v == "--quickPlaySingleplayer")));
    if uid == "net.fabricmc.fabric-loader" {
      assert!(resolved
        .client
        .libraries
        .iter()
        .any(|l| l.name.starts_with("net.fabricmc:intermediary:1.20.1")));
    } else {
      assert!(profile
        .maven_files
        .iter()
        .any(|a| a.path.ends_with("-installer.jar")));
    }
    println!(
      "{uid} {version}: entry={main}, libraries={}, download-only={}",
      resolved.client.libraries.len(),
      profile.maven_files.len()
    );
  }
}
