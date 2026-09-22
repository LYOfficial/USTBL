use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::game_version::compare_game_versions;
use crate::instance::models::misc::Instance;
use crate::launch::models::LaunchError;
use crate::launcher_config::models::{GameJava, JavaInfo};
use std::cmp::Ordering;
use tauri::AppHandle;

/// Component profiles declare a set, not a minimum. Java 18 is not implicitly
/// compatible with a profile accepting [17, 21], and explicit user choices must
/// be checked too. Keep this pure so selection is deterministic and testable.
pub fn select_component_java(
  game_java: &GameJava,
  java_list: &[JavaInfo],
  accepted: &[i32],
) -> USTBLResult<JavaInfo> {
  if !game_java.auto {
    let selected = java_list
      .iter()
      .find(|j| j.exec_path == game_java.exec_path)
      .ok_or(LaunchError::SelectedJavaUnavailable)?;
    if accepted.contains(&selected.major_version) {
      return Ok(selected.clone());
    }
    return Err(USTBLError(format!("Selected Java {} is incompatible; this instance requires one of {:?}. Select a compatible Java or enable automatic selection.", selected.major_version, accepted)));
  }
  java_list.iter().filter(|j| accepted.contains(&j.major_version))
    .min_by(|a, b| {
      accepted.iter().position(|v| *v == a.major_version).cmp(&accepted.iter().position(|v| *v == b.major_version))
        .then_with(|| b.name.cmp(&a.name))
        .then_with(|| a.exec_path.cmp(&b.exec_path))
    }).cloned().ok_or_else(|| USTBLError(format!("No compatible Java installed; this instance requires one of {:?}. Install or add a compatible runtime in Java settings.", accepted)))
}

pub async fn select_java_runtime(
  app: &AppHandle,
  game_java: &GameJava,
  java_list: &[JavaInfo],
  instance: &Instance,
  client_json_req: i32,
  // TODO: pass client and mod loader info to calculate version with more rules, instead of passing require version
  // ref: https://github.com/Hex-Dragon/PCL2/blob/16e09c792ce8c13435fc6827e6da54170aaa3bc0/Plain%20Craft%20Launcher%202/Modules/Minecraft/ModLaunch.vb#L1130
) -> USTBLResult<JavaInfo> {
  if !game_java.auto {
    return java_list
      .iter()
      .find(|j| j.exec_path == game_java.exec_path)
      .cloned()
      .ok_or_else(|| LaunchError::SelectedJavaUnavailable.into());
  }

  let mut min_version_req = get_minimum_java_version_by_game(app, instance).await;

  if client_json_req > min_version_req {
    min_version_req = client_json_req;
  }

  let mut suitable_candidates = Vec::new();
  for java in java_list {
    match java.major_version.cmp(&min_version_req) {
      Ordering::Equal => return Ok(java.clone()),
      Ordering::Greater => suitable_candidates.push(java.clone()),
      _ => {}
    }
  }

  if suitable_candidates.is_empty() {
    Err(LaunchError::NoSuitableJava.into())
  } else {
    suitable_candidates.sort_by_key(|j| j.major_version);
    Ok(suitable_candidates[0].clone())
  }
}

/// Get minimum java version requirement by game client version
/// ref: https://zh.minecraft.wiki/w/Java%E7%89%88?variant=zh-cn#%E8%BD%AF%E4%BB%B6%E9%9C%80%E6%B1%82
async fn get_minimum_java_version_by_game(app: &AppHandle, instance: &Instance) -> i32 {
  // only allow fallback remote fetch here in the launch process, as Java selection and command generation are used sequentially.
  // ref: https://github.com/USTB-SkyCode/USTBL/pull/799
  // 26.1(26.1-snapshot-1)+
  if compare_game_versions(app, &instance.version, "26.1-snapshot-1", true).await >= Ordering::Equal
  {
    return 25;
  }
  // 1.20.5(24w14a)+
  if compare_game_versions(app, &instance.version, "24w14a", false).await >= Ordering::Equal {
    return 21;
  }
  // 1.18(1.18-pre2)+
  if compare_game_versions(app, &instance.version, "1.18-pre2", false).await >= Ordering::Equal {
    return 17;
  }
  // 1.17(21w19a)+
  if compare_game_versions(app, &instance.version, "21w19a", false).await >= Ordering::Equal {
    return 16;
  }
  // 1.12(17w13a)+
  if compare_game_versions(app, &instance.version, "17w13a", false).await >= Ordering::Equal {
    return 8;
  }
  0
}

#[cfg(test)]
mod tests {
  use super::*;
  fn runtime(major: i32) -> JavaInfo {
    JavaInfo {
      major_version: major,
      exec_path: format!("java{major}"),
      ..Default::default()
    }
  }
  #[test]
  fn component_java_is_an_explicit_set_not_a_minimum() {
    let config = GameJava {
      auto: true,
      ..Default::default()
    };
    let list = [runtime(8), runtime(18), runtime(21), runtime(26)];
    assert_eq!(
      select_component_java(&config, &list, &[17, 21, 25])
        .unwrap()
        .major_version,
      21
    );
    assert!(select_component_java(&config, &list[..2], &[17, 21]).is_err());
    assert_eq!(
      select_component_java(&config, &[runtime(21), runtime(17)], &[17, 21])
        .unwrap()
        .major_version,
      17
    );
  }
  #[test]
  fn explicit_incompatible_java_is_reported_before_spawning() {
    let config = GameJava {
      auto: false,
      exec_path: "java8".into(),
    };
    assert!(
      select_component_java(&config, &[runtime(8), runtime(21)], &[17, 21])
        .unwrap_err()
        .0
        .contains("Selected Java 8")
    );
  }
}
