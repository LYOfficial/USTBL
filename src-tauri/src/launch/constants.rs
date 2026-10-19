pub const GAME_PROCESS_OUTPUT_EVENT: &str = "launch:game-process-output";

/// Substrings that, when seen in the game process output, indicate the game has
/// started far enough that the launch process is finished and the frontend modal
/// can be dismissed.
///
/// NOTE: these are matched against the game's **stdout/stderr**, not the log file
/// written by the game itself. On modern versions (1.13+) and especially on modded
/// loaders such as NeoForge/Forge (which replace the log format and shorten thread
/// names), the classic Mojang markers below never show up on stdout:
///
///   - "render thread"  -> the thread is logged as `Render thread` in the *file*
///                         log, but NeoForge's stdout uses short names like `main`.
///   - "lwjgl version"  -> only printed by very old (<= 1.12) versions.
///   - "lwjgl openal"   -> same, legacy only.
///
/// Keeping those as the only markers makes the launcher stall on a NeoForge
/// instance: `launch_game` waits for the readiness signal while the "waiting for
/// game to launch" modal stays on screen. The entries below cover both the legacy
/// format and the modern one so that every loader is handled.
pub const READY_FLAG: &[&str] = &[
  // ---- legacy (<= 1.12): Mojang used to print these straight to stdout ----
  "render thread",
  "lwjgl version",
  "lwjgl openal",
  // ---- modern vanilla / Forge / NeoForge / Fabric ----
  // NeoForge's early display prints the GL context it created once the graphics
  // stack is up. Also emitted by vanilla 1.13+. Seen on NeoForge 20.x-26.x.
  "gl info:",
  // The Mixin subsystem announces itself once the transformer service is live;
  // this covers loaders that swap out the early display.
  "mixin subsystem",
];
