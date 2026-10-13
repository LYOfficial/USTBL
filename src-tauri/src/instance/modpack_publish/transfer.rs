//! Upload bookkeeping: bytes sent, speed, and how many files go up at once.

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const START_LEVEL: usize = 2;
const MAX_LEVEL: usize = 6;
/// Another file in flight must lift the total speed by this much to be kept.
const GAIN_TO_KEEP: f64 = 1.15;
const SPEED_WINDOW: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTransfer {
  pub name: String,
  pub done: u64,
  pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transfer {
  pub bytes_done: u64,
  pub bytes_total: u64,
  /// Bytes per second over the last few seconds.
  pub speed: u64,
  pub concurrency: usize,
  /// Files being sent right now.
  pub files: Vec<FileTransfer>,
}

/// One file on its way up. Counts the bytes handed to the HTTP stack, so it
/// runs slightly ahead of what the server has taken, and reads 100% while the
/// server still relays the file on to cloud storage.
pub struct ActiveFile {
  name: String,
  total: u64,
  sent: AtomicU64,
}

impl ActiveFile {
  pub fn new(name: String, total: u64) -> Arc<Self> {
    Arc::new(Self {
      name,
      total,
      sent: AtomicU64::new(0),
    })
  }

  /// A retried request (after a token refresh) sends the file from the start.
  pub fn restart(&self) {
    self.sent.store(0, Ordering::Relaxed);
  }

  pub fn add(&self, bytes: u64) {
    self.sent.fetch_add(bytes, Ordering::Relaxed);
  }

  fn done(&self) -> u64 {
    self.sent.load(Ordering::Relaxed).min(self.total)
  }
}

pub struct Tracker {
  bytes_total: u64,
  finished: AtomicU64,
  active: Mutex<Vec<Arc<ActiveFile>>>,
  samples: Mutex<VecDeque<(Instant, u64)>>,
}

impl Tracker {
  pub fn new(bytes_total: u64) -> Self {
    Self {
      bytes_total,
      finished: AtomicU64::new(0),
      active: Mutex::new(Vec::new()),
      samples: Mutex::new(VecDeque::new()),
    }
  }

  pub fn begin(&self, file: &Arc<ActiveFile>) {
    self.active.lock().unwrap().push(file.clone());
  }

  /// Takes the file off the list; a stored file counts in full.
  pub fn end(&self, file: &Arc<ActiveFile>, stored: bool) {
    self
      .active
      .lock()
      .unwrap()
      .retain(|item| !Arc::ptr_eq(item, file));
    if stored {
      self.finished.fetch_add(file.total, Ordering::Relaxed);
    }
  }

  pub fn bytes_done(&self) -> u64 {
    let active: u64 = self
      .active
      .lock()
      .unwrap()
      .iter()
      .map(|file| file.done())
      .sum();
    (self.finished.load(Ordering::Relaxed) + active).min(self.bytes_total)
  }

  pub fn snapshot(&self, concurrency: usize) -> Transfer {
    let now = Instant::now();
    let bytes_done = self.bytes_done();
    let speed = {
      let mut samples = self.samples.lock().unwrap();
      samples.push_back((now, bytes_done));
      while samples.len() > 2 && now.duration_since(samples[1].0) >= SPEED_WINDOW {
        samples.pop_front();
      }
      speed_of(&samples)
    };
    let files = self
      .active
      .lock()
      .unwrap()
      .iter()
      .map(|file| FileTransfer {
        name: file.name.clone(),
        done: file.done(),
        total: file.total,
      })
      .collect();
    Transfer {
      bytes_done,
      bytes_total: self.bytes_total,
      speed,
      concurrency,
      files,
    }
  }
}

fn speed_of(samples: &VecDeque<(Instant, u64)>) -> u64 {
  let (Some(first), Some(last)) = (samples.front(), samples.back()) else {
    return 0;
  };
  let seconds = last.0.duration_since(first.0).as_secs_f64();
  if seconds <= 0.0 {
    return 0;
  }
  (last.1.saturating_sub(first.1) as f64 / seconds) as u64
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
    assert_eq!(level.level(), 2);
    level.observe(1_000_000.0);
    assert_eq!(level.level(), 3);
    level.observe(1_600_000.0);
    assert_eq!(level.level(), 4);
  }

  #[test]
  fn keeps_the_last_helpful_count_when_the_gain_stops() {
    let mut level = Concurrency::new();
    level.observe(1_000_000.0);
    level.observe(1_800_000.0);
    assert_eq!(level.level(), 4);
    level.observe(1_850_000.0);
    assert_eq!(level.level(), 3);
    // Settled: later readings no longer move it.
    level.observe(9_000_000.0);
    assert_eq!(level.level(), 3);
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
  fn tracker_counts_stored_files_in_full_and_active_ones_by_bytes() {
    let tracker = Tracker::new(300);
    let first = ActiveFile::new("a".into(), 100);
    let second = ActiveFile::new("b".into(), 200);
    tracker.begin(&first);
    tracker.begin(&second);
    first.add(100);
    second.add(50);
    assert_eq!(tracker.bytes_done(), 150);

    tracker.end(&first, true);
    assert_eq!(tracker.bytes_done(), 150);
    // A failed file stops counting.
    tracker.end(&second, false);
    assert_eq!(tracker.bytes_done(), 100);
  }

  #[test]
  fn a_restarted_file_counts_from_zero_again() {
    let tracker = Tracker::new(100);
    let file = ActiveFile::new("a".into(), 100);
    tracker.begin(&file);
    file.add(80);
    file.restart();
    assert_eq!(tracker.bytes_done(), 0);
    file.add(500);
    // Never past the file's size.
    assert_eq!(tracker.bytes_done(), 100);
  }

  #[test]
  fn speed_is_bytes_over_the_sampled_time() {
    let start = Instant::now();
    let mut samples = VecDeque::new();
    samples.push_back((start, 0));
    samples.push_back((start + Duration::from_secs(2), 2_000_000));
    assert_eq!(speed_of(&samples), 1_000_000);
    // Counters that went backwards read as no speed, not a huge number.
    samples.push_back((start + Duration::from_secs(3), 10));
    assert_eq!(speed_of(&samples), 3);
  }
}
