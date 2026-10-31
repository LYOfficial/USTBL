//! Batched lookups that fill gaps source by source.
//!
//! A batch endpoint answers `200` with only the entries it knows, and a mirror
//! such as MCIM may not know entries it has not cached yet. So a key missing from
//! one source's reply is not yet "does not exist": it is asked again, in one
//! request, from the next source.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::hash::Hash;

/// What a lookup learned about a set of keys. A key in neither field could not
/// be answered (every request that would have settled it failed).
#[derive(Debug, Clone)]
pub struct LookupOutcome<K, V> {
  pub found: HashMap<K, V>,
  /// Keys the last source answered without. Only then is a key known to be missing.
  pub not_found: HashSet<K>,
}

impl<K, V> Default for LookupOutcome<K, V> {
  fn default() -> Self {
    Self {
      found: HashMap::new(),
      not_found: HashSet::new(),
    }
  }
}

impl<K: Hash + Eq, V> LookupOutcome<K, V> {
  pub fn extend(&mut self, other: Self) {
    self.found.extend(other.found);
    self.not_found.extend(other.not_found);
  }
}

/// Ask `sources` in order. Each source receives only the keys still missing, as a
/// single batch. `ask` returns `None` when the request failed; the keys then stay
/// open for the next source.
///
/// Keys are settled as missing only when the last source answered successfully
/// and did not have them; if it failed, they stay unanswered, so the caller
/// does not remember them as missing.
pub async fn lookup_across_sources<S, K, V, F, Fut>(
  sources: &[S],
  keys: &[K],
  ask: F,
) -> LookupOutcome<K, V>
where
  S: Copy,
  K: Hash + Eq + Clone,
  F: Fn(S, Vec<K>) -> Fut,
  Fut: Future<Output = Option<HashMap<K, V>>>,
{
  let mut outcome = LookupOutcome::default();
  let mut remaining: Vec<K> = keys.to_vec();
  let mut last_answered = false;

  for source in sources {
    if remaining.is_empty() {
      break;
    }
    match ask(*source, remaining.clone()).await {
      Some(mut reply) => {
        last_answered = true;
        remaining.retain(|key| match reply.remove(key) {
          Some(value) => {
            outcome.found.insert(key.clone(), value);
            false
          }
          None => true,
        });
      }
      None => last_answered = false,
    }
  }

  if last_answered {
    outcome.not_found.extend(remaining);
  }
  outcome
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Mutex;

  type Reply = Option<Vec<&'static str>>;

  /// Source 0 and 1 answer with the given known keys, or fail on `None`.
  async fn run(
    replies: [Reply; 2],
    keys: &[&'static str],
  ) -> (LookupOutcome<&'static str, u8>, Vec<(u8, usize)>) {
    let calls = Mutex::new(Vec::new());
    let keys: Vec<&'static str> = keys.to_vec();
    let outcome = lookup_across_sources(&[0usize, 1], &keys, |source, asked: Vec<&'static str>| {
      calls.lock().unwrap().push((source as u8, asked.len()));
      let reply = replies[source].clone();
      async move {
        reply.map(|known| {
          asked
            .into_iter()
            .filter(|key| known.contains(key))
            .map(|key| (key, source as u8))
            .collect()
        })
      }
    })
    .await;
    let calls = calls.into_inner().unwrap();
    (outcome, calls)
  }

  #[tokio::test]
  async fn the_next_source_fills_what_the_first_one_lacks() {
    let (outcome, calls) = run([Some(vec!["a"]), Some(vec!["b"])], &["a", "b", "c"]).await;
    assert_eq!(outcome.found[&"a"], 0);
    assert_eq!(outcome.found[&"b"], 1);
    // Only "c" is known to be missing, and the second source was asked only for the gaps.
    assert_eq!(outcome.not_found, HashSet::from(["c"]));
    assert_eq!(calls, vec![(0, 3), (1, 2)]);
  }

  #[tokio::test]
  async fn a_complete_first_answer_needs_no_second_request() {
    let (outcome, calls) = run([Some(vec!["a", "b"]), Some(vec![])], &["a", "b"]).await;
    assert_eq!(outcome.found.len(), 2);
    assert!(outcome.not_found.is_empty());
    assert_eq!(calls, vec![(0, 2)]);
  }

  #[tokio::test]
  async fn a_failed_first_source_hands_everything_to_the_next() {
    let (outcome, calls) = run([None, Some(vec!["a"])], &["a", "b"]).await;
    assert_eq!(outcome.found[&"a"], 1);
    assert_eq!(outcome.not_found, HashSet::from(["b"]));
    assert_eq!(calls, vec![(0, 2), (1, 2)]);
  }

  #[tokio::test]
  async fn missing_everywhere_is_not_found() {
    let (outcome, _) = run([Some(vec![]), Some(vec![])], &["a"]).await;
    assert!(outcome.found.is_empty());
    assert_eq!(outcome.not_found, HashSet::from(["a"]));
  }

  #[tokio::test]
  async fn a_failed_last_source_leaves_the_gaps_unanswered() {
    let (outcome, _) = run([Some(vec!["a"]), None], &["a", "b"]).await;
    assert_eq!(outcome.found[&"a"], 0);
    assert!(outcome.not_found.is_empty());
  }

  #[tokio::test]
  async fn all_sources_failing_answers_nothing() {
    let (outcome, calls) = run([None, None], &["a", "b"]).await;
    assert!(outcome.found.is_empty() && outcome.not_found.is_empty());
    assert_eq!(calls.len(), 2);
  }
}
