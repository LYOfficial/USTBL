// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tokio::main(flavor = "multi_thread")]
async fn main() {
  let exit_code = app_lib::run().await;
  #[cfg(debug_assertions)]
  eprintln!("Launcher event loop returned {exit_code}");
  std::process::exit(exit_code);
}
