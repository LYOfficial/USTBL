//! Bytes sent and the number of simultaneous uploads while publishing.

use crate::tasks::streams::desc::PStatus;
use crate::tasks::PTaskHandle;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

// Each file also waits for the platform to hand it on to cloud storage, so the
// network sits idle part of the time and more files in flight pay off.
const START_LEVEL: usize = 4;
const MAX_LEVEL: usize = 8;
/// Another file in flight must lift the total speed by this much to be kept.
const GAIN_TO_KEEP: f64 = 1.15;

/// Bytes of one file handed to the HTTP stack, and of all files together.
///
/// This runs slightly ahead of what the server has taken, and reads 100% while
/// the server still relays the file on to cloud storage.
pub struct ByteCounter {
  total: u64,
  sent: AtomicU64,
  all: Arc<AtomicU64>,
}

impl ByteCounter {
  fn new(total: u64, all: Arc<AtomicU64>) -> Self {
    Self {
      total,
      sent: AtomicU64::new(0),
      all,
    }
  }

  /// A request sent again (after a token refresh) starts the file over.
  fn restart(&self) {
    self.sent.store(0, Ordering::Relaxed);
  }

  fn add(&self, bytes: u64) {
    self.sent.fetch_add(bytes, Ordering::Relaxed);
    self.all.fetch_add(bytes, Ordering::Relaxed);
  }

  fn done(&self) -> u64 {
    self.sent.load(Ordering::Relaxed).min(self.total)
  }
}

/// One file on its way up, tied to its entry on the task list.
pub struct ActiveFile {
  handle: Arc<RwLock<PTaskHandle>>,
  bytes: ByteCounter,
  started: Instant,
  /// When the last byte was handed over.
  sent_at: Mutex<Option<Instant>>,
}

impl ActiveFile {
  /// Called when the body has been read to its end.
  pub fn mark_sent(&self) {
    *self.sent_at.lock().unwrap() = Some(Instant::now());
  }

  /// How long sending took, and how long the platform took to answer after.
  pub fn timings(&self) -> (Duration, Duration) {
    let now = Instant::now();
    match *self.sent_at.lock().unwrap() {
      Some(sent) => (sent.duration_since(self.started), now.duration_since(sent)),
      None => (now.duration_since(self.started), Duration::ZERO),
    }
  }

  pub fn restart(&self) {
    self.bytes.restart();
  }

  pub fn add(&self, bytes: u64) {
    self.bytes.add(bytes);
  }

  pub fn status(&self) -> PStatus {
    self.handle.read().unwrap().status().clone()
  }
}

/// The files being sent right now.
pub struct Transfers {
  all: Arc<AtomicU64>,
  active: Mutex<Vec<Arc<ActiveFile>>>,
}

impl Transfers {
  pub fn new() -> Self {
    Self {
      all: Arc::new(AtomicU64::new(0)),
      active: Mutex::new(Vec::new()),
    }
  }

  /// Everything handed to the network so far, retried bytes included.
  pub fn sent_bytes(&self) -> u64 {
    self.all.load(Ordering::Relaxed)
  }

  pub fn begin(&self, handle: &Arc<RwLock<PTaskHandle>>) -> Arc<ActiveFile> {
    let total = handle.read().unwrap().desc.total.max(0) as u64;
    let file = Arc::new(ActiveFile {
      handle: handle.clone(),
      bytes: ByteCounter::new(total, self.all.clone()),
      started: Instant::now(),
      sent_at: Mutex::new(None),
    });
    self.active.lock().unwrap().push(file.clone());
    file
  }

  pub fn end(&self, file: &Arc<ActiveFile>) {
    self
      .active
      .lock()
      .unwrap()
      .retain(|item| !Arc::ptr_eq(item, file));
  }

  /// Tells the task list how far every running file got. Call once a second:
  /// the speed shown is bytes per reporting interval.
  pub fn report(&self) {
    for file in self.active.lock().unwrap().iter() {
      let mut handle = file.handle.write().unwrap();
      if !handle.status().is_in_progress() {
        continue;
      }
      let current = file.bytes.done() as i64;
      handle.desc.current = current;
      let (id, group) = (handle.desc.task_id, handle.desc.task_group.clone());
      handle
        .reporter
        .report_progress(id, group.as_deref(), current);
    }
  }
}
/// Feels its way to a good number of simultaneous uploads: adds one at a time
/// while the total speed keeps rising, then keeps the last count that helped.
#[derive(Debug)]
pub struct Concurrency {
  level: usize,
  best_speed: f64,
  settled: bool,
}

impl Concurrency {
  pub fn new() -> Self {
    Self {
      level: START_LEVEL,
      best_speed: 0.0,
      settled: false,
    }
  }

  pub fn level(&self) -> usize {
    self.level
  }

  /// `speed` is the total bytes per second measured while `level` files were
  /// in flight.
  pub fn observe(&mut self, speed: f64) {
    if self.settled {
      return;
    }
    if speed > self.best_speed * GAIN_TO_KEEP {
      self.best_speed = speed;
      if self.level < MAX_LEVEL {
        self.level += 1;
      } else {
        self.settled = true;
      }
    } else {
      // The last file added did not pay off.
      self.level = (self.level - 1).max(1);
      self.settled = true;
    }
  }

  /// A failure while several files were in flight: go one at a time from here.
  pub fn back_off(&mut self) {
    self.level = 1;
    self.settled = true;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn adds_files_while_the_speed_keeps_rising() {
    let mut level = Concurrency::new();
    assert_eq!(level.level(), START_LEVEL);
    level.observe(1_000_000.0);
    assert_eq!(level.level(), START_LEVEL + 1);
    level.observe(1_600_000.0);
    assert_eq!(level.level(), START_LEVEL + 2);
  }

  #[test]
  fn keeps_the_last_helpful_count_when_the_gain_stops() {
    let mut level = Concurrency::new();
    level.observe(1_000_000.0);
    level.observe(1_800_000.0);
    assert_eq!(level.level(), START_LEVEL + 2);
    level.observe(1_850_000.0);
    assert_eq!(level.level(), START_LEVEL + 1);
    // Settled: later readings no longer move it.
    level.observe(9_000_000.0);
    assert_eq!(level.level(), START_LEVEL + 1);
  }

  #[test]
  fn stops_at_the_cap() {
    let mut level = Concurrency::new();
    let mut speed = 1_000_000.0;
    for _ in 0..10 {
      level.observe(speed);
      speed *= 2.0;
    }
    assert_eq!(level.level(), MAX_LEVEL);
  }

  #[test]
  fn a_failure_drops_to_one_file_at_a_time() {
    let mut level = Concurrency::new();
    level.back_off();
    assert_eq!(level.level(), 1);
    level.observe(5_000_000.0);
    assert_eq!(level.level(), 1);
  }

  #[test]
  fn a_file_counts_up_to_its_size_and_all_files_add_up() {
    let all = Arc::new(AtomicU64::new(0));
    let first = ByteCounter::new(100, all.clone());
    let second = ByteCounter::new(200, all.clone());
    first.add(100);
    second.add(50);
    assert_eq!(first.done(), 100);
    assert_eq!(second.done(), 50);
    assert_eq!(all.load(Ordering::Relaxed), 150);

    // Never past the file's size.
    first.add(30);
    assert_eq!(first.done(), 100);
  }

  #[test]
  fn a_restarted_file_counts_from_zero_but_the_total_keeps_the_traffic() {
    let all = Arc::new(AtomicU64::new(0));
    let file = ByteCounter::new(100, all.clone());
    file.add(80);
    file.restart();
    assert_eq!(file.done(), 0);
    assert_eq!(all.load(Ordering::Relaxed), 80);
  }
}
