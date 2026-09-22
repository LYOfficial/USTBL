use dotenvy::{dotenv_override, from_filename};
use std::path::Path;
use std::{env, fs};

fn main() {
  // Unit tests now exercise the download pipeline, which links Windows dialogs.
  // Tauri embeds this dependency in the application manifest, but not in Rust's
  // test harness; use the same common-controls version for the test harness.
  if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
    && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
  {
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'");
    // The app already has Tauri's resource manifest; do not embed a second one.
    println!("cargo:rustc-link-arg-bin=USTBL=/MANIFEST:NO");
  }

  if std::env::var("GITHUB_ACTIONS").is_err() {
    // Load env variables from ".env" file, if not exists, use ".env.template" to set default value.
    from_filename(".env.template").ok();
    dotenv_override().ok();
  }

  let out_dir = env::var("OUT_DIR").unwrap_or_else(|_| "".to_string());
  let dest_path = Path::new(&out_dir).join("secrets.rs");
  let _ = fs::remove_file(&dest_path);

  // Iterate over all env variables and print those starting with "USTBL_" for compilation (env variables can not be accessed directly in compile time)
  // ref: https://users.rust-lang.org/t/std-set-var-in-build-rs-not-setting-environment-variable/34924/6
  // original naive impl, see: https://github.com/USTB-SkyCode/USTBL/pull/412/files
  for (key, value) in env::vars() {
    if key.starts_with("USTBL_") {
      println!("cargo:rustc-env={}={}", key, value);
    }
  }

  // Notify Cargo to auto re-run the build script if .env changes
  println!("cargo:rerun-if-changed=.env");
  println!("cargo:rerun-if-changed=.env.template");

  tauri_build::build()
}
