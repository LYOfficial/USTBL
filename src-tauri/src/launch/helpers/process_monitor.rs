use crate::error::USTBLResult;
use crate::instance::models::misc::{Instance, InstanceError};
use crate::launch::constants::*;
use crate::launch::helpers::playtime_sync;
use crate::launch::models::{LaunchError, LaunchingState};
use crate::launcher_config::models::{LauncherVisiablity, ProcessPriority};
use crate::utils::shell::execute_command_line;
use crate::utils::window::create_webview_window;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::prelude::*;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;
use std::{fs, thread};
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Emitter, Manager};
use tokio;
use tokio::sync::Notify;

const POLLING_OPERATION_INTERVAL_MS: u64 = 2000;
const PLAY_TIME_SAVE_INTERVAL_SECONDS: u64 = 600;
const INSTANCE_PLAY_TIME_UPDATED_EVENT: &str = "instance:play-time-updated";
static PLAY_TIME_WRITE_LOCK: LazyLock<tokio::sync::Mutex<()>> =
  LazyLock::new(|| tokio::sync::Mutex::new(()));

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstancePlayTimeUpdated {
  instance_id: String,
  play_time: u128,
}

struct OutputPipe<T: Read + Send + 'static> {
  app: AppHandle,
  out: T,
  label: String,
  start_time: Arc<Mutex<Option<Instant>>>,
  log_file: Arc<Mutex<File>>,
  display_log_window: bool,
  ready_tx: Sender<()>,
  game_ready_flag: Arc<AtomicBool>,
}

impl<T: Read + Send + 'static> OutputPipe<T> {
  fn listen_from_output(self) -> thread::JoinHandle<()> {
    thread::spawn(move || {
      let reader = BufReader::new(self.out);
      for line in reader.lines().map_while(Result::ok) {
        if self.display_log_window {
          let _ = self
            .app
            .emit_to(&self.label, GAME_PROCESS_OUTPUT_EVENT, &line);
        }
        writeln!(self.log_file.lock().unwrap(), "{line}").unwrap();
        // the first time when log contains 'render thread', 'lwjgl version', or 'lwjgl openal', send signal to launch command, close frontend modal.
        if !self.game_ready_flag.load(Ordering::SeqCst)
          && READY_FLAG.iter().any(|p| line.to_lowercase().contains(p))
        {
          self.game_ready_flag.store(true, Ordering::SeqCst);
          // record Instant::now as game start time
          let mut start_time_lock = self.start_time.lock().unwrap();
          if start_time_lock.is_none() {
            *start_time_lock = Some(Instant::now());
          }
          let _ = self.ready_tx.send(());
        }
      }
    })
  }
}

fn play_time_checkpoint(elapsed_seconds: u64, finishing: bool) -> u64 {
  if finishing {
    elapsed_seconds
  } else {
    elapsed_seconds / PLAY_TIME_SAVE_INTERVAL_SECONDS * PLAY_TIME_SAVE_INTERVAL_SECONDS
  }
}

async fn record_play_time_delta(
  app: &AppHandle,
  instance_id: &str,
  delta_seconds: u64,
) -> USTBLResult<u128> {
  let _write_guard = PLAY_TIME_WRITE_LOCK.lock().await;
  let instance_in_mem = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let instance = binding
      .lock()?
      .get(instance_id)
      .cloned()
      .ok_or(InstanceError::InstanceNotFoundByID)?;
    instance
  };

  let mut instance = instance_in_mem
    .load_json_cfg()
    .await
    .unwrap_or(instance_in_mem);
  instance.play_time = instance.play_time.saturating_add(u128::from(delta_seconds));
  instance.save_json_cfg().await?;
  let play_time = instance.play_time;

  let binding = app.state::<Mutex<HashMap<String, Instance>>>();
  if let Ok(mut instances) = binding.lock() {
    if let Some(current) = instances.get_mut(instance_id) {
      current.play_time = play_time;
    }
  }

  let _ = app.emit(
    INSTANCE_PLAY_TIME_UPDATED_EVENT,
    InstancePlayTimeUpdated {
      instance_id: instance_id.to_string(),
      play_time,
    },
  );
  Ok(play_time)
}

pub async fn monitor_process(
  app: AppHandle,
  id: u64,
  mut child: Child,
  instance_id: String,
  display_log_window: bool,
  custom_title: &str,
  launcher_visibility: LauncherVisiablity,
  ready_tx: Sender<()>,
  post_exit_command: Option<String>,
) -> USTBLResult<()> {
  // create unique log window
  let label = format!("game_log_{id}");
  let log_file_path = app
    .path()
    .resolve::<PathBuf>(format!("game/{label}.log").into(), BaseDirectory::AppLog)?;
  if let Some(parent_dir) = log_file_path.parent() {
    fs::create_dir_all(parent_dir)?;
  }

  let log_file = Arc::new(Mutex::new(
    std::fs::OpenOptions::new()
      .create_new(true)
      .write(true)
      .read(true)
      .open(&log_file_path)?,
  ));

  let log_window = if display_log_window {
    create_webview_window(&app, &label, "game_log", None)
      .await
      .ok()
  } else {
    None
  };

  let game_ready_flag = Arc::new(AtomicBool::new(false));
  let start_time: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None)); // used to calculate play time
  let instance_name = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    binding.lock().ok().and_then(|instances| {
      instances
        .get(&instance_id)
        .map(|instance| instance.name.clone())
    })
  };
  let playtime_stop = Arc::new(Notify::new());
  let playtime_task = {
    let app = app.clone();
    let instance_id = instance_id.clone();
    let start_time = start_time.clone();
    let playtime_stop = playtime_stop.clone();
    tokio::spawn(async move {
      let mut persisted_seconds = 0_u64;
      let mut reported_seconds = 0_u64;
      loop {
        let finishing = tokio::select! {
          _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => false,
          _ = playtime_stop.notified() => true,
        };
        let elapsed_seconds = start_time
          .lock()
          .ok()
          .and_then(|start| *start)
          .map(|start| start.elapsed().as_secs())
          .unwrap_or(0);
        let checkpoint = play_time_checkpoint(elapsed_seconds, finishing);
        let persist_seconds = checkpoint.saturating_sub(persisted_seconds);
        if persist_seconds > 0 {
          match record_play_time_delta(&app, &instance_id, persist_seconds).await {
            Ok(_) => persisted_seconds = checkpoint,
            Err(error) => log::warn!("Failed to persist instance play time: {error:?}"),
          }
        }

        let sync_seconds = checkpoint.saturating_sub(reported_seconds);
        if sync_seconds > 0
          && playtime_sync::queue_playtime_delta(
            &app,
            &instance_id,
            instance_name.as_deref(),
            sync_seconds,
          )
          .await
          .is_ok()
        {
          reported_seconds = reported_seconds.saturating_add(sync_seconds);
        }
        if finishing {
          break;
        }
      }
    })
  };

  let stdout = child.stdout.take().map(|out| {
    (OutputPipe {
      app: app.clone(),
      label: label.clone(),
      out,
      start_time: start_time.clone(),
      log_file: log_file.clone(),
      display_log_window,
      ready_tx: ready_tx.clone(),
      game_ready_flag: game_ready_flag.clone(),
    })
    .listen_from_output()
  });

  // handle game process stderr
  let stderr = child.stderr.take().map(|out| {
    (OutputPipe {
      app: app.clone(),
      label: label.clone(),
      out,
      start_time: start_time.clone(),
      log_file: log_file.clone(),
      display_log_window,
      ready_tx: ready_tx.clone(),
      game_ready_flag: game_ready_flag.clone(),
    })
    .listen_from_output()
  });

  // polling thread (for changing window title, etc.)
  let stop_polling_flag = Arc::new(AtomicBool::new(false));
  {
    let stop_polling_flag = stop_polling_flag.clone();
    let pid = child.id();
    let custom_title = custom_title.to_string();
    thread::spawn(move || {
      while !stop_polling_flag.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(
          POLLING_OPERATION_INTERVAL_MS,
        ));
        let _ = change_process_window_title(pid, &custom_title).is_err();
      }
    });
  };

  // handle game process exit
  let game_ready_flag = game_ready_flag.clone();
  let stop_polling_flag = stop_polling_flag.clone();

  tokio::spawn(async move {
    let exit_ok = match child.wait() {
      Ok(status) => {
        if let Some(h) = stdout {
          let _ = h.join();
        }

        if let Some(h) = stderr {
          let _ = h.join();
        }

        if !game_ready_flag.load(Ordering::SeqCst) {
          false
        } else {
          log_file.lock().unwrap().flush().unwrap();
          status.success()
        }
      }

      Err(e) => {
        writeln!(
          log_file.lock().unwrap(),
          "[FATAL] Game process was killed Reason: {e}."
        )
        .unwrap();
        false
      }
    };

    stop_polling_flag.store(true, Ordering::SeqCst);
    playtime_stop.notify_one();
    let _ = playtime_task.await;
    crate::account::helpers::vustb_presence::track_game_stopped(id);
    let presence_app = app.clone();
    tauri::async_runtime::spawn(async move {
      if let Err(error) = crate::account::helpers::vustb_presence::sync(&presence_app).await {
        log::debug!("vUSTB presence update after game exit skipped: {error:?}");
      }
    });
    drop(log_file);
    // handle launcher main window visiablity
    match launcher_visibility {
      LauncherVisiablity::RunningHidden => {
        let main_window = app.get_webview_window("main").expect("no main window");
        let _ = main_window.show();
        let _ = main_window.set_focus();
      }
      LauncherVisiablity::StartHidden => {
        // If the main window is still hidden (not shown again due to the single instance plugin when the user runs the launcher again), exit the launcher process
        let main_window = app.get_webview_window("main").expect("no main window");
        if let Ok(is_visible) = main_window.is_visible() {
          if !is_visible {
            std::process::exit(0);
          }
        } else {
          std::process::exit(0);
        }
      }
      _ => {}
    }

    if exit_ok {
      if let Some(ref window) = log_window {
        let _ = window.destroy();
      }

      let launching_queue_state = app.state::<Mutex<Vec<LaunchingState>>>();
      let mut launching_queue = launching_queue_state.lock().unwrap();
      launching_queue.retain(|state| state.id != id);
    } else {
      let launching_option = {
        let launching_queue_state = app.state::<Mutex<Vec<LaunchingState>>>();
        let launching_queue = launching_queue_state.lock().unwrap();
        launching_queue.iter().find(|s| s.id == id).cloned()
      };

      if let Some(launching) = launching_option {
        if launching.current_step == 0 {
          // it was marked as manually cancelled, then remove from launching_queue and not show game error window
          let launching_queue_state = app.state::<Mutex<Vec<LaunchingState>>>();
          let mut launching_queue = launching_queue_state.lock().unwrap();
          launching_queue.retain(|state| state.id != id);
        } else {
          let _ = create_webview_window(&app, &format!("game_error_{id}"), "game_error", None)
            .await
            .unwrap();
        }
      }
    }

    if let Some(cmdline) = post_exit_command.as_ref().filter(|s| !s.trim().is_empty()) {
      let _ = execute_command_line(cmdline);
    }
  });

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::play_time_checkpoint;

  #[test]
  fn running_play_time_is_persisted_at_ten_minute_boundaries() {
    assert_eq!(play_time_checkpoint(0, false), 0);
    assert_eq!(play_time_checkpoint(599, false), 0);
    assert_eq!(play_time_checkpoint(600, false), 600);
    assert_eq!(play_time_checkpoint(1201, false), 1200);
  }

  #[test]
  fn exiting_game_persists_the_remaining_seconds() {
    assert_eq!(play_time_checkpoint(61, true), 61);
    assert_eq!(play_time_checkpoint(1201, true), 1201);
  }
}

pub fn kill_process(pid: u32) -> USTBLResult<()> {
  // KNOWN ISSUE: kill process means exit abnormally, which will not close the game-log window automatically.
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  {
    Command::new("kill")
      .args(["-9", &pid.to_string()])
      .output()
      .map_err(|_| LaunchError::KillProcessFailed)?;
  }

  #[cfg(target_os = "windows")]
  {
    use std::os::windows::process::CommandExt;

    Command::new("taskkill")
      .args(["/F", "/T", "/PID", &pid.to_string()])
      .creation_flags(0x08000000) // CREATE_NO_WINDOW
      .output()
      .map_err(|_| LaunchError::KillProcessFailed)?;
  }

  Ok(())
}

pub fn set_process_priority(pid: u32, priority: &ProcessPriority) -> USTBLResult<()> {
  #[cfg(any(target_os = "macos", target_os = "linux"))]
  {
    let nice_value = match *priority {
      ProcessPriority::Low => 5,
      ProcessPriority::BelowNormal => 1,
      ProcessPriority::Normal => 0,
      // FIXME: above normal need permission
      ProcessPriority::AboveNormal => -1,
      ProcessPriority::High => -5,
    };

    let _ = Command::new("renice")
      .args([nice_value.to_string(), "-p".to_string(), pid.to_string()])
      .output()
      .map_err(|_| LaunchError::SetProcessPriorityFailed)?;
  }

  #[cfg(target_os = "windows")]
  {
    use winapi::shared::minwindef::{DWORD, FALSE};
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::{OpenProcess, SetPriorityClass};
    use winapi::um::winbase::{
      ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS,
      IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    };
    use winapi::um::winnt::PROCESS_SET_INFORMATION;

    unsafe {
      let h_process = OpenProcess(PROCESS_SET_INFORMATION, FALSE, pid as DWORD);
      if h_process.is_null() {
        return Err(LaunchError::SetProcessPriorityFailed.into());
      }

      let priority_class = match *priority {
        ProcessPriority::Low => IDLE_PRIORITY_CLASS,
        ProcessPriority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
        ProcessPriority::Normal => NORMAL_PRIORITY_CLASS,
        ProcessPriority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
        ProcessPriority::High => HIGH_PRIORITY_CLASS,
      };

      if SetPriorityClass(h_process, priority_class) == 0 {
        CloseHandle(h_process);
        return Err(LaunchError::SetProcessPriorityFailed.into());
      }

      CloseHandle(h_process);
    }
  }

  Ok(())
}

pub fn change_process_window_title(pid: u32, new_title: &str) -> USTBLResult<()> {
  if new_title.trim().is_empty() {
    return Ok(());
  }
  #[cfg(target_os = "windows")]
  {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use winapi::shared::minwindef::{BOOL, DWORD, LPARAM, TRUE};
    use winapi::shared::windef::HWND;
    use winapi::um::winnt::LPCWSTR;
    use winapi::um::winuser::{EnumWindows, GetWindowThreadProcessId, SetWindowTextW};
    let new_title = new_title.to_string();
    let closure = |hwnd: HWND| unsafe {
      let mut window_pid: DWORD = 0;
      GetWindowThreadProcessId(hwnd, &mut window_pid);
      if window_pid == pid {
        let new_title: Vec<u16> = OsStr::new(&new_title)
          .encode_wide()
          .chain(once(0))
          .collect();
        SetWindowTextW(hwnd, new_title.as_ptr() as LPCWSTR);
      }
    };
    type ForEachCallback<'a> = Box<dyn FnMut(HWND) + 'a>;
    let wrapper: ForEachCallback = Box::new(closure);
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
      if let Some(boxed) = (lparam as *mut ForEachCallback).as_mut() {
        (*boxed)(hwnd);
      }
      TRUE
    }
    unsafe {
      EnumWindows(Some(enum_proc), &wrapper as *const _ as LPARAM);
    }
  }

  #[cfg(any(target_os = "macos", target_os = "linux"))]
  {
    // not support yet.
    let _ = (pid, new_title, LaunchError::ChangeWindowTitleFailed); // avoid unused warning
  }

  Ok(())
}
