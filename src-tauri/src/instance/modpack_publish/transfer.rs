//! Progress of the bytes a publish sends.

use crate::tasks::PTaskHandle;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Bytes of the pack on the platform plus those on their way, for one upload.
///
/// Counts what was handed to the HTTP stack, so it runs slightly ahead of what
/// the server has taken; a chunk that fails is taken back out.
#[derive(Debug)]
pub struct SentBytes {
  total: u64,
  sent: AtomicU64,
}

impl SentBytes {
  pub fn new(total: u64, already_stored: u64) -> Arc<Self> {
    Arc::new(Self {
      total,
      sent: AtomicU64::new(already_stored.min(total)),
    })
  }

  pub fn add(&self, bytes: u64) {
    self.sent.fetch_add(bytes, Ordering::Relaxed);
  }

  pub fn take_back(&self, bytes: u64) {
    let _ = self
      .sent
      .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |sent| {
        Some(sent.saturating_sub(bytes))
      });
  }

  pub fn done(&self) -> u64 {
    self.sent.load(Ordering::Relaxed).min(self.total)
  }
}

/// Tells the task list how far the upload got. Call once a second: the speed
/// shown is bytes per reporting interval.
pub fn report(handle: &Arc<RwLock<PTaskHandle>>, current: u64) {
  let mut handle = handle.write().unwrap();
  if !handle.status().is_in_progress() {
    return;
  }
  let current = current as i64;
  handle.desc.current = current;
  let (id, group) = (handle.desc.task_id, handle.desc.task_group.clone());
  handle
    .reporter
    .report_progress(id, group.as_deref(), current);
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn counts_up_to_the_total_and_takes_failed_bytes_back() {
    let sent = SentBytes::new(100, 40);
    assert_eq!(sent.done(), 40);
    sent.add(50);
    assert_eq!(sent.done(), 90);
    sent.add(30);
    assert_eq!(sent.done(), 100);
    sent.take_back(60);
    assert_eq!(sent.done(), 60);
    sent.take_back(1000);
    assert_eq!(sent.done(), 0);
  }

  #[test]
  fn what_is_stored_already_never_exceeds_the_total() {
    assert_eq!(SentBytes::new(10, 50).done(), 10);
  }
}
