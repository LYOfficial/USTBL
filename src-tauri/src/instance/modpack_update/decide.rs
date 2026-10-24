//! The update decision of one managed path, as a pure function.
//!
//! Given the remote's policy for the path (ship / ignore / delete /
//! unmentioned), the baseline entry and what sits on disk, `decide` answers
//! three things: what to do with the file, whether the player has to answer a
//! conflict, and what the next baseline entry is. It never touches the disk:
//! the caller feeds it comparison keys and reads the actions out of the
//! result. A first install is the same decision with an empty baseline.
//!
//! The content rules are the tables of 《整合包发布三态与本地忽略-方案》§5.1
//! and §5.3, the disabled-state rules §5.2. The remote side of a shipped file
//! arrives through [`ShipInput`], so `decide` itself stays free of remote
//! plumbing. A managed file whose remote also ships it goes through the
//! existing three-way comparison instead (`decide_ship_managed`).

use super::diff::FileKey;
use crate::instance::helpers::modpack::baseline::BaselineFile;

/// What the new version says about one path.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Policy {
  /// It ships the file.
  Ship,
  /// The file is left alone and its baseline entry goes dormant.
  Ignore,
  /// The file is deleted, whatever it contains.
  Delete,
  /// Nothing is said; an unmentioned managed file is removed.
  Unmentioned,
}

/// The remote's side of a shipped file. `disabled: None` means the remote has
/// no opinion on the disabled state (Modrinth/CurseForge); `once` marks a
/// file that is only ever written by the first install.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ShipInput {
  pub key: FileKey,
  pub disabled: Option<bool>,
  pub once: bool,
}

/// The baseline entry of the path.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Base {
  None,
  Managed {
    key: FileKey,
    disabled: bool,
  },
  Dormant {
    key: Option<FileKey>,
    disabled: bool,
  },
}

/// What is on disk for the path. `disabled` says which filename variant the
/// content sits at: `false` for `<path>`, `true` for `<path>.disabled`.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Local {
  Absent,
  Present {
    key: FileKey,
    disabled: bool,
  },
  /// Both variants exist at once; keys are (enabled, disabled).
  Both {
    enabled: FileKey,
    disabled: FileKey,
  },
}

/// Everything one path's decision needs.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct DecisionInput<'a> {
  pub policy: Policy,
  /// The remote key; present exactly when `policy` is `Ship`.
  pub remote: Option<&'a ShipInput>,
  pub base: Base,
  pub local: Local,
  /// The local content counts as unchanged even when its key differs (the
  /// local-ignore verdict; U3 feeds it, until then it is always `false`).
  pub effectively_unchanged: bool,
}

/// Which filename variant the file ends up at after the decision.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Variant {
  Enabled,
  Disabled,
}

impl Variant {
  pub fn of_disabled(disabled: bool) -> Self {
    if disabled {
      Variant::Disabled
    } else {
      Variant::Enabled
    }
  }

  /// Whether this is the `.disabled` variant.
  pub fn disabled(&self) -> bool {
    matches!(self, Variant::Disabled)
  }
}

/// One concrete step on disk, before conflicts are answered.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Action {
  /// Write the remote content at this variant. The content is fetched from
  /// the new version (or comes from an automatic merge).
  WriteRemote {
    key: FileKey,
    variant: Variant,
  },
  /// Move the local file to the other variant without changing its content.
  Rename {
    key: FileKey,
    variant: Variant,
  },
  /// Back up and delete both variants.
  Delete,
  /// The file goes dormant or its entry goes away; nothing is written.
  Retire,
  Nothing,
}

/// Why the player has to answer this path, as far as `decide` can tell on its
/// own. The caller maps these onto the conflict kinds of the update plan; a
/// text file's `Modified` conflict may still merge block by block there.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DecisionConflict {
  /// A file the modpack never recorded sits where the new version wants one.
  UnmanagedFileInTheWay,
  /// A dormant file is managed again while the local content is neither the
  /// recorded nor the shipped one. Only a `/3` remote with a dormant local
  /// produces it; the compiler never sees one on this branch's own paths.
  #[allow(dead_code)]
  Remanage,
  /// Edited here, but the new version no longer ships it.
  ModifiedRemovedRemotely,
  /// Deleted here, but the new version changed it.
  DeletedChangedRemotely,
  /// Edited here and changed by the new version.
  ModifiedChangedRemotely,
}

/// What the baseline records for the path after the update.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum NextBaseline {
  /// The entry stays exactly as it is.
  Unchanged,
  /// The entry goes away.
  Remove,
  /// The entry becomes this.
  Set(BaselineFile),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Decision {
  pub action: Action,
  pub conflict: Option<DecisionConflict>,
  pub next: NextBaseline,
}

/// A decision that leaves file, answer and baseline untouched.
const UNTOUCHED: Decision = Decision {
  action: Action::Nothing,
  conflict: None,
  next: NextBaseline::Unchanged,
};

fn managed(path: &str, key: &FileKey, disabled: bool) -> NextBaseline {
  NextBaseline::Set(BaselineFile::managed(path, key.clone(), disabled))
}

fn dormant(path: &str, key: Option<FileKey>, disabled: bool) -> NextBaseline {
  NextBaseline::Set(BaselineFile::dormant(path, key, disabled))
}

/// The disabled state the file ends up with, by §5.2. Only consulted for
/// shipped files. `base_disabled` is the recorded value of a managed entry or
/// a dormant one with content; `local_disabled` the variant on disk, if any.
/// A conflict leaves the local file alone, so its variant survives.
pub fn disabled_outcome(
  remote: Option<bool>,
  base_disabled: Option<bool>,
  local_disabled: Option<bool>,
) -> bool {
  disabled_outcome_inner(remote, base_disabled, local_disabled, false)
}

/// The same three-way, with the conflict case spelled out: when the player is
/// asked, the file on disk is not renamed before they answer.
pub fn disabled_outcome_conflicted(
  remote: Option<bool>,
  base_disabled: Option<bool>,
  local_disabled: Option<bool>,
) -> bool {
  disabled_outcome_inner(remote, base_disabled, local_disabled, true)
}

fn disabled_outcome_inner(
  remote: Option<bool>,
  base_disabled: Option<bool>,
  local_disabled: Option<bool>,
  conflicted: bool,
) -> bool {
  if conflicted {
    if let Some(local) = local_disabled {
      return local;
    }
  }
  match remote {
    Some(remote) => match base_disabled {
      Some(base) => match local_disabled {
        Some(local) if local != base => local,
        _ => remote,
      },
      None => local_disabled.unwrap_or(remote),
    },
    None => local_disabled.unwrap_or(false),
  }
}

/// The baseline's disabled value after the update: the remote's when it has
/// an opinion, the surviving local value otherwise.
pub fn disabled_record(remote: Option<bool>, local_disabled: Option<bool>) -> bool {
  remote.or(local_disabled).unwrap_or(false)
}

/// The baseline's disabled value while the player is asked: a conflict keeps
/// the local file, so the entry records the local variant when one exists.
pub fn disabled_record_conflicted(
  remote: Option<bool>,
  base_disabled: Option<bool>,
  local_disabled: Option<bool>,
) -> bool {
  match local_disabled {
    Some(local) => local,
    None => disabled_record(remote, base_disabled),
  }
}

/// The key and variant of the local file. `Both` reads as the enabled one:
/// the caller errors out on `Both` before deciding whenever the remote or a
/// managed baseline involves the path.
fn local_parts(local: &Local) -> (Option<FileKey>, Option<bool>) {
  match local {
    Local::Absent => (None, None),
    Local::Present { key, disabled } => (Some(key.clone()), Some(*disabled)),
    Local::Both { enabled, .. } => (Some(enabled.clone()), Some(false)),
  }
}

/// One path's update decision. `path` only labels the next baseline entry.
pub fn decide(path: &str, input: &DecisionInput) -> Decision {
  match input.policy {
    Policy::Ship => {
      let ship = input.remote.expect("a ship policy carries the remote key");
      if let Base::Managed { key, disabled } = &input.base {
        let (key, disabled) = (key.clone(), *disabled);
        decide_ship_managed(path, ship, key, disabled, input)
      } else if ship.once {
        decide_ship_once(path, ship, input)
      } else {
        decide_ship_unmanaged(path, ship, input)
      }
    }
    Policy::Ignore => decide_ignore(path, input),
    Policy::Delete => decide_delete(input),
    Policy::Unmentioned => decide_unmentioned(path, input),
  }
}

/// Ship × (no baseline, or a dormant one): §5.1 rows 1-3 and 5-7. Whether the
/// file is written, and to which variant, follows the remote key and §5.2.
fn decide_ship_unmanaged(path: &str, ship: &ShipInput, input: &DecisionInput) -> Decision {
  let (local_key, local_disabled) = local_parts(&input.local);
  let base_disabled = match &input.base {
    Base::Dormant { key, disabled } => key.as_ref().map(|_| *disabled),
    _ => None,
  };
  let outcome = disabled_outcome(ship.disabled, base_disabled, local_disabled);
  let record = disabled_record(ship.disabled, local_disabled);
  // A conflict keeps the local file where it sits; the player answers.
  let conflicted_outcome =
    disabled_outcome_conflicted(ship.disabled, base_disabled, local_disabled);
  let conflicted_record = disabled_record_conflicted(ship.disabled, base_disabled, local_disabled);
  let next = || managed(path, &ship.key, record);
  let conflicted_next = || managed(path, &ship.key, conflicted_record);
  let write_or_rename = |key: &FileKey| {
    if local_disabled == Some(outcome) {
      Action::Nothing
    } else {
      Action::Rename {
        key: key.clone(),
        variant: Variant::of_disabled(outcome),
      }
    }
  };
  match &local_key {
    None => Decision {
      action: Action::WriteRemote {
        key: ship.key.clone(),
        variant: Variant::of_disabled(outcome),
      },
      conflict: None,
      next: next(),
    },
    Some(key) if *key == ship.key => Decision {
      action: write_or_rename(key),
      conflict: None,
      next: next(),
    },
    Some(key) => match &input.base {
      Base::Dormant {
        key: Some(base_key),
        ..
      } if base_key == key => Decision {
        action: Action::WriteRemote {
          key: ship.key.clone(),
          variant: Variant::of_disabled(outcome),
        },
        conflict: None,
        next: next(),
      },
      Base::Dormant { key: Some(_), .. } => Decision {
        action: Action::Nothing,
        conflict: Some(DecisionConflict::Remanage),
        next: conflicted_next(),
      },
      _ => Decision {
        action: if Some(conflicted_outcome) == local_disabled {
          Action::Nothing
        } else {
          Action::Rename {
            key: ship.key.clone(),
            variant: Variant::of_disabled(conflicted_outcome),
          }
        },
        conflict: Some(DecisionConflict::UnmanagedFileInTheWay),
        next: conflicted_next(),
      },
    },
  }
}

/// §5.3: a `once` file is only ever written by the first install; afterwards
/// it goes dormant with its content, and the player may even delete it.
fn decide_ship_once(path: &str, ship: &ShipInput, input: &DecisionInput) -> Decision {
  let (local_key, local_disabled) = local_parts(&input.local);
  match &input.base {
    Base::Dormant { .. } => UNTOUCHED,
    Base::Managed { .. } => unreachable!("handled by decide_ship_managed"),
    Base::None => match &local_key {
      None => {
        let variant = Variant::of_disabled(ship.disabled.unwrap_or(false));
        Decision {
          action: Action::WriteRemote {
            key: ship.key.clone(),
            variant,
          },
          conflict: None,
          next: dormant(path, Some(ship.key.clone()), ship.disabled.unwrap_or(false)),
        }
      }
      Some(key) if *key == ship.key => Decision {
        action: Action::Nothing,
        conflict: None,
        next: dormant(
          path,
          Some(ship.key.clone()),
          local_disabled.unwrap_or(false),
        ),
      },
      Some(_) => Decision {
        action: Action::Nothing,
        conflict: None,
        next: dormant(path, None, false),
      },
    },
  }
}

/// Ship × managed baseline. The content side answers like the existing
/// three-way comparison (the whole-file conflicts of §6.1); text block merges
/// and their choices stay with the caller. A `once` file never moves again:
/// its entry goes dormant with the recorded content.
fn decide_ship_managed(
  path: &str,
  ship: &ShipInput,
  base_key: FileKey,
  base_disabled: bool,
  input: &DecisionInput,
) -> Decision {
  if ship.once {
    return Decision {
      action: Action::Retire,
      conflict: None,
      next: dormant(path, Some(base_key), base_disabled),
    };
  }
  let (local_key, local_disabled) = local_parts(&input.local);
  let outcome = disabled_outcome(ship.disabled, Some(base_disabled), local_disabled);
  let record = disabled_record(ship.disabled, local_disabled);
  let next = managed(path, &ship.key, record);
  // The disabled variant the file should end up at, when it keeps its content.
  let settle_variant = |key: &FileKey| {
    if local_disabled == Some(outcome) {
      Action::Nothing
    } else {
      Action::Rename {
        key: key.clone(),
        variant: Variant::of_disabled(outcome),
      }
    }
  };
  match &local_key {
    None => Decision {
      action: Action::Nothing,
      // A file the player deleted stays deleted unless the new version
      // changed it; then the caller asks.
      conflict: (ship.key != base_key).then_some(DecisionConflict::DeletedChangedRemotely),
      next,
    },
    Some(key) => {
      if ship.key == base_key {
        // The remote did not touch the file: the local edit stays, only the
        // disabled state may move, and the entry records the remote's opinion.
        Decision {
          action: settle_variant(key),
          conflict: None,
          next,
        }
      } else if *key == base_key {
        Decision {
          action: Action::WriteRemote {
            key: ship.key.clone(),
            variant: Variant::of_disabled(outcome),
          },
          conflict: None,
          next,
        }
      } else if *key == ship.key {
        Decision {
          action: settle_variant(key),
          conflict: None,
          next,
        }
      } else {
        Decision {
          action: Action::Nothing,
          conflict: Some(DecisionConflict::ModifiedChangedRemotely),
          next,
        }
      }
    }
  }
}

fn decide_ignore(path: &str, input: &DecisionInput) -> Decision {
  match &input.base {
    Base::Managed { key, disabled } => Decision {
      action: Action::Retire,
      conflict: None,
      next: dormant(path, Some(key.clone()), *disabled),
    },
    _ => UNTOUCHED,
  }
}

fn decide_delete(input: &DecisionInput) -> Decision {
  Decision {
    action: if matches!(input.local, Local::Absent) {
      Action::Nothing
    } else {
      Action::Delete
    },
    conflict: None,
    next: NextBaseline::Remove,
  }
}

fn decide_unmentioned(path: &str, input: &DecisionInput) -> Decision {
  let _ = path;
  let Base::Managed { key, .. } = &input.base else {
    return UNTOUCHED;
  };
  let (local_key, _) = local_parts(&input.local);
  match &local_key {
    None => Decision {
      action: Action::Nothing,
      conflict: None,
      next: NextBaseline::Remove,
    },
    Some(local) if local == key || input.effectively_unchanged => Decision {
      action: Action::Delete,
      conflict: None,
      next: NextBaseline::Remove,
    },
    Some(_) => Decision {
      action: Action::Nothing,
      conflict: Some(DecisionConflict::ModifiedRemovedRemotely),
      next: NextBaseline::Remove,
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn key(tag: &str) -> FileKey {
    FileKey {
      sha1: tag.into(),
      size: tag.len() as u64,
    }
  }

  const R: &str = "remote";
  const B: &str = "base";
  const L: &str = "local";
  const P: &str = "p";

  fn ship(tag: &str, disabled: Option<bool>) -> ShipInput {
    ShipInput {
      key: key(tag),
      disabled,
      once: false,
    }
  }

  fn ship_once(tag: &str, disabled: Option<bool>) -> ShipInput {
    ShipInput {
      key: key(tag),
      disabled,
      once: true,
    }
  }

  fn input<'a>(
    policy: Policy,
    remote: Option<&'a ShipInput>,
    base: Base,
    local: Local,
  ) -> DecisionInput<'a> {
    DecisionInput {
      policy,
      remote,
      base,
      local,
      effectively_unchanged: false,
    }
  }

  fn write_remote(tag: &str, disabled: bool) -> Action {
    Action::WriteRemote {
      key: key(tag),
      variant: Variant::of_disabled(disabled),
    }
  }

  fn rename(tag: &str, disabled: bool) -> Action {
    Action::Rename {
      key: key(tag),
      variant: Variant::of_disabled(disabled),
    }
  }

  fn managed_entry(tag: &str, disabled: bool) -> NextBaseline {
    managed(P, &key(tag), disabled)
  }

  fn dormant_entry(tag: Option<&str>, disabled: bool) -> NextBaseline {
    dormant(P, tag.map(key), disabled)
  }

  fn decision(action: Action, conflict: Option<DecisionConflict>, next: NextBaseline) -> Decision {
    Decision {
      action,
      conflict,
      next,
    }
  }

  const SHIP_RD: Policy = Policy::Ship;

  // ---- §5.1: ship × (no baseline | contentless dormant) ---------------

  #[test]
  fn ship_without_baseline_installs_when_absent() {
    for base in [
      Base::None,
      Base::Dormant {
        key: None,
        disabled: false,
      },
    ] {
      for (rd, want_variant) in [(Some(false), false), (Some(true), true), (None, false)] {
        let remote = ship(R, rd);
        let actual = decide(
          P,
          &input(SHIP_RD, Some(&remote), base.clone(), Local::Absent),
        );
        assert_eq!(
          actual,
          decision(
            write_remote(R, want_variant),
            None,
            managed_entry(R, rd.unwrap_or(false))
          ),
          "rd={rd:?}"
        );
      }
    }
  }

  #[test]
  fn ship_without_baseline_does_not_rewrite_matching_content() {
    for base in [
      Base::None,
      Base::Dormant {
        key: None,
        disabled: true,
      },
    ] {
      for variant in [false, true] {
        let remote = ship(R, None);
        let actual = decide(
          P,
          &input(
            SHIP_RD,
            Some(&remote),
            base.clone(),
            Local::Present {
              key: key(R),
              disabled: variant,
            },
          ),
        );
        // The remote has no opinion, so the local variant is kept and recorded.
        assert_eq!(
          actual,
          decision(Action::Nothing, None, managed_entry(R, variant))
        );
      }
      // The remote asks for disabled while the file sits enabled: without a
      // recorded state the local variant wins, so nothing moves but the
      // baseline records the remote's opinion.
      let remote = ship(R, Some(true));
      let actual = decide(
        P,
        &input(
          SHIP_RD,
          Some(&remote),
          base.clone(),
          Local::Present {
            key: key(R),
            disabled: false,
          },
        ),
      );
      assert_eq!(
        actual,
        decision(Action::Nothing, None, managed_entry(R, true))
      );
    }
    // With a recorded state the local one matches, the remote's applies: the
    // file flips to its disabled name.
    let remote = ship(R, Some(true));
    let actual = decide(
      P,
      &input(
        SHIP_RD,
        Some(&remote),
        Base::Dormant {
          key: Some(key(R)),
          disabled: false,
        },
        Local::Present {
          key: key(R),
          disabled: false,
        },
      ),
    );
    assert_eq!(
      actual,
      decision(rename(R, true), None, managed_entry(R, true))
    );
  }

  #[test]
  fn ship_without_baseline_conflicts_on_a_foreign_file() {
    for base in [
      Base::None,
      Base::Dormant {
        key: None,
        disabled: false,
      },
    ] {
      for variant in [false, true] {
        let remote = ship(R, Some(false));
        let actual = decide(
          P,
          &input(
            SHIP_RD,
            Some(&remote),
            base.clone(),
            Local::Present {
              key: key(L),
              disabled: variant,
            },
          ),
        );
        assert_eq!(
          actual,
          decision(
            Action::Nothing,
            Some(DecisionConflict::UnmanagedFileInTheWay),
            managed_entry(R, variant)
          ),
          "variant={variant}"
        );
      }
    }
  }

  // ---- §5.1: ship × dormant with content --------------------------------

  #[test]
  fn ship_over_content_dormant_installs_or_stays_or_updates_or_conflicts() {
    let dormant_base = || Base::Dormant {
      key: Some(key(B)),
      disabled: false,
    };
    // L absent → install.
    let remote = ship(R, Some(false));
    assert_eq!(
      decide(
        P,
        &input(SHIP_RD, Some(&remote), dormant_base(), Local::Absent)
      ),
      decision(write_remote(R, false), None, managed_entry(R, false))
    );
    // L ≡ R → nothing.
    assert_eq!(
      decide(
        P,
        &input(
          SHIP_RD,
          Some(&remote),
          dormant_base(),
          Local::Present {
            key: key(R),
            disabled: false,
          },
        ),
      ),
      decision(Action::Nothing, None, managed_entry(R, false))
    );
    // L ≡ B → update to R, no prompt.
    assert_eq!(
      decide(
        P,
        &input(
          SHIP_RD,
          Some(&remote),
          dormant_base(),
          Local::Present {
            key: key(B),
            disabled: false,
          },
        ),
      ),
      decision(write_remote(R, false), None, managed_entry(R, false))
    );
    // L other → remanage conflict.
    assert_eq!(
      decide(
        P,
        &input(
          SHIP_RD,
          Some(&remote),
          dormant_base(),
          Local::Present {
            key: key(L),
            disabled: false,
          },
        ),
      ),
      decision(
        Action::Nothing,
        Some(DecisionConflict::Remanage),
        managed_entry(R, false)
      )
    );
  }

  // ---- §5.1: ship × managed ----------------------------------------------

  #[test]
  fn ship_over_managed_keeps_local_edits_when_the_remote_did_not_move() {
    let managed_base = |d| Base::Managed {
      key: key(B),
      disabled: d,
    };
    let remote = ship(B, None);
    for variant in [false, true] {
      let actual = decide(
        P,
        &input(
          SHIP_RD,
          Some(&remote),
          managed_base(false),
          Local::Present {
            key: key(L),
            disabled: variant,
          },
        ),
      );
      // Content stays; only the recorded disabled follows the local variant.
      assert_eq!(
        actual,
        decision(Action::Nothing, None, managed_entry(B, variant))
      );
    }
  }

  #[test]
  fn ship_over_managed_updates_an_untouched_file_and_merges_disabled() {
    let managed_base = |d| Base::Managed {
      key: key(B),
      disabled: d,
    };
    // Local ≡ B, remote asks for disabled: write the new file disabled.
    let remote = ship(R, Some(true));
    assert_eq!(
      decide(
        P,
        &input(
          SHIP_RD,
          Some(&remote),
          managed_base(false),
          Local::Present {
            key: key(B),
            disabled: false,
          },
        ),
      ),
      decision(write_remote(R, true), None, managed_entry(R, true))
    );
    // The player already disabled it themselves: the variant stays local's.
    let remote = ship(R, Some(false));
    assert_eq!(
      decide(
        P,
        &input(
          SHIP_RD,
          Some(&remote),
          managed_base(false),
          Local::Present {
            key: key(B),
            disabled: true,
          },
        ),
      ),
      decision(write_remote(R, true), None, managed_entry(R, false))
    );
  }

  #[test]
  fn ship_over_managed_leaves_a_deleted_file_alone_unless_the_remote_moved() {
    let managed_base = || Base::Managed {
      key: key(B),
      disabled: false,
    };
    let same = ship(B, None);
    assert_eq!(
      decide(
        P,
        &input(SHIP_RD, Some(&same), managed_base(), Local::Absent)
      ),
      decision(Action::Nothing, None, managed_entry(B, false))
    );
    let moved = ship(R, None);
    assert_eq!(
      decide(
        P,
        &input(SHIP_RD, Some(&moved), managed_base(), Local::Absent)
      ),
      decision(
        Action::Nothing,
        Some(DecisionConflict::DeletedChangedRemotely),
        managed_entry(R, false)
      )
    );
  }

  #[test]
  fn ship_over_managed_recognizes_local_matches_and_conflicts_the_rest() {
    let managed_base = || Base::Managed {
      key: key(B),
      disabled: false,
    };
    let remote = ship(R, None);
    // Already at the new content.
    assert_eq!(
      decide(
        P,
        &input(
          SHIP_RD,
          Some(&remote),
          managed_base(),
          Local::Present {
            key: key(R),
            disabled: false,
          },
        ),
      ),
      decision(Action::Nothing, None, managed_entry(R, false))
    );
    // Changed on both sides.
    assert_eq!(
      decide(
        P,
        &input(
          SHIP_RD,
          Some(&remote),
          managed_base(),
          Local::Present {
            key: key(L),
            disabled: false,
          },
        ),
      ),
      decision(
        Action::Nothing,
        Some(DecisionConflict::ModifiedChangedRemotely),
        managed_entry(R, false)
      )
    );
  }

  #[test]
  fn ship_over_managed_once_only_retires_the_entry() {
    let managed_base = |d| Base::Managed {
      key: key(B),
      disabled: d,
    };
    let remote = ship_once(R, Some(true));
    for base_disabled in [false, true] {
      for local in [
        Local::Absent,
        Local::Present {
          key: key(B),
          disabled: false,
        },
        Local::Present {
          key: key(L),
          disabled: true,
        },
      ] {
        let actual = decide(
          P,
          &input(SHIP_RD, Some(&remote), managed_base(base_disabled), local),
        );
        assert_eq!(
          actual,
          decision(Action::Retire, None, dormant_entry(Some(B), base_disabled))
        );
      }
    }
  }

  // ---- §5.1: ignore / delete / unmentioned --------------------------------

  #[test]
  fn ignore_retires_a_managed_entry_and_leaves_the_rest() {
    let remote_none: Option<&ShipInput> = None;
    let managed_base = Base::Managed {
      key: key(B),
      disabled: true,
    };
    for local in [
      Local::Absent,
      Local::Present {
        key: key(B),
        disabled: false,
      },
      Local::Present {
        key: key(L),
        disabled: true,
      },
    ] {
      assert_eq!(
        decide(
          P,
          &input(Policy::Ignore, remote_none, managed_base.clone(), local)
        ),
        decision(Action::Retire, None, dormant_entry(Some(B), true))
      );
    }
    for base in [
      Base::None,
      Base::Dormant {
        key: Some(key(B)),
        disabled: false,
      },
    ] {
      assert_eq!(
        decide(P, &input(Policy::Ignore, remote_none, base, Local::Absent)),
        UNTOUCHED
      );
    }
  }

  #[test]
  fn delete_removes_whichever_variant_exists() {
    let remote_none: Option<&ShipInput> = None;
    for base in [
      Base::None,
      Base::Managed {
        key: key(B),
        disabled: false,
      },
      Base::Dormant {
        key: None,
        disabled: false,
      },
    ] {
      for (local, action) in [
        (Local::Absent, Action::Nothing),
        (
          Local::Present {
            key: key(L),
            disabled: false,
          },
          Action::Delete,
        ),
        (
          Local::Present {
            key: key(L),
            disabled: true,
          },
          Action::Delete,
        ),
        (
          Local::Both {
            enabled: key(L),
            disabled: key(B),
          },
          Action::Delete,
        ),
      ] {
        assert_eq!(
          decide(P, &input(Policy::Delete, remote_none, base.clone(), local)),
          decision(action, None, NextBaseline::Remove)
        );
      }
    }
  }

  #[test]
  fn unmentioned_removes_a_managed_entry_by_the_players_answer() {
    let remote_none: Option<&ShipInput> = None;
    let managed_base = || Base::Managed {
      key: key(B),
      disabled: false,
    };
    // Absent: nothing to delete, the entry still goes.
    assert_eq!(
      decide(
        P,
        &input(
          Policy::Unmentioned,
          remote_none,
          managed_base(),
          Local::Absent
        )
      ),
      decision(Action::Nothing, None, NextBaseline::Remove)
    );
    // Untouched content: deleted without asking.
    for variant in [false, true] {
      assert_eq!(
        decide(
          P,
          &input(
            Policy::Unmentioned,
            remote_none,
            managed_base(),
            Local::Present {
              key: key(B),
              disabled: variant,
            },
          ),
        ),
        decision(Action::Delete, None, NextBaseline::Remove),
        "variant={variant}"
      );
    }
    // Edited: the conflict decides; the entry goes either way.
    assert_eq!(
      decide(
        P,
        &input(
          Policy::Unmentioned,
          remote_none,
          managed_base(),
          Local::Present {
            key: key(L),
            disabled: false,
          },
        ),
      ),
      decision(
        Action::Nothing,
        Some(DecisionConflict::ModifiedRemovedRemotely),
        NextBaseline::Remove
      )
    );
    // Only the disabled state differs from the baseline: it still counts as
    // unchanged content and is deleted without a prompt.
    let managed_disabled = Base::Managed {
      key: key(B),
      disabled: true,
    };
    assert_eq!(
      decide(
        P,
        &input(
          Policy::Unmentioned,
          remote_none,
          managed_disabled,
          Local::Present {
            key: key(B),
            disabled: false,
          },
        ),
      ),
      decision(Action::Delete, None, NextBaseline::Remove)
    );
  }

  #[test]
  fn unmentioned_leaves_dormant_and_unknown_files_alone() {
    let remote_none: Option<&ShipInput> = None;
    for base in [
      Base::None,
      Base::Dormant {
        key: Some(key(B)),
        disabled: false,
      },
      Base::Dormant {
        key: None,
        disabled: false,
      },
    ] {
      for local in [
        Local::Absent,
        Local::Present {
          key: key(L),
          disabled: false,
        },
      ] {
        assert_eq!(
          decide(
            P,
            &input(Policy::Unmentioned, remote_none, base.clone(), local)
          ),
          UNTOUCHED
        );
      }
    }
  }

  // ---- §5.3: once ---------------------------------------------------------

  #[test]
  fn once_installs_records_or_remembers_without_writing() {
    let remote = ship_once(R, Some(true));
    // Nothing there: install disabled, remember as dormant with content.
    assert_eq!(
      decide(P, &input(SHIP_RD, Some(&remote), Base::None, Local::Absent)),
      decision(write_remote(R, true), None, dormant_entry(Some(R), true))
    );
    // Already there with the same content: nothing to write; the local
    // variant is recorded.
    for variant in [false, true] {
      assert_eq!(
        decide(
          P,
          &input(
            SHIP_RD,
            Some(&remote),
            Base::None,
            Local::Present {
              key: key(R),
              disabled: variant,
            },
          ),
        ),
        decision(Action::Nothing, None, dormant_entry(Some(R), variant))
      );
    }
    // Something else sits there: keep it, remember "seen" without content.
    assert_eq!(
      decide(
        P,
        &input(
          SHIP_RD,
          Some(&remote),
          Base::None,
          Local::Present {
            key: key(L),
            disabled: false,
          },
        ),
      ),
      decision(Action::Nothing, None, dormant_entry(None, false))
    );
  }

  #[test]
  fn once_never_moves_a_dormant_file_again() {
    let remote = ship_once(R, None);
    for base in [
      Base::Dormant {
        key: Some(key(R)),
        disabled: true,
      },
      Base::Dormant {
        key: None,
        disabled: false,
      },
    ] {
      for local in [
        Local::Absent,
        Local::Present {
          key: key(R),
          disabled: false,
        },
        Local::Present {
          key: key(L),
          disabled: true,
        },
      ] {
        assert_eq!(
          decide(P, &input(SHIP_RD, Some(&remote), base.clone(), local)),
          UNTOUCHED
        );
      }
    }
  }

  // ---- §5.2: disabled-state three-way --------------------------------------

  #[test]
  fn disabled_outcome_is_a_no_conflict_three_way() {
    // The remote has no opinion: the local variant wins; a fresh install is
    // enabled.
    assert_eq!(disabled_outcome(None, Some(false), Some(true)), true);
    assert_eq!(disabled_outcome(None, Some(true), Some(false)), false);
    assert_eq!(disabled_outcome(None, None, Some(true)), true);
    assert_eq!(disabled_outcome(None, None, None), false);
    // The remote has an opinion and the player kept the recorded state: the
    // remote wins. The player changed it: theirs wins.
    assert_eq!(disabled_outcome(Some(true), Some(false), Some(false)), true);
    assert_eq!(disabled_outcome(Some(false), Some(false), Some(true)), true);
    assert_eq!(disabled_outcome(Some(true), Some(false), Some(true)), true);
    assert_eq!(disabled_outcome(Some(true), Some(false), None), true);
    // The baseline never recorded it: the local variant wins, else the
    // remote's.
    assert_eq!(disabled_outcome(Some(true), None, Some(false)), false);
    assert_eq!(disabled_outcome(Some(true), None, None), true);
  }

  // ---- the exhaustive oracle -----------------------------------------------

  fn all_keys() -> [FileKey; 4] {
    [key("k0"), key("k1"), key("k2"), key("k3")]
  }

  fn all_ship_inputs() -> Vec<ShipInput> {
    let mut inputs = Vec::new();
    for key in all_keys() {
      for disabled in [None, Some(false), Some(true)] {
        for once in [false, true] {
          inputs.push(ShipInput {
            key: key.clone(),
            disabled,
            once,
          });
        }
      }
    }
    inputs
  }

  fn all_bases() -> Vec<Base> {
    let mut bases = vec![Base::None];
    for key in all_keys() {
      for disabled in [false, true] {
        bases.push(Base::Managed {
          key: key.clone(),
          disabled,
        });
      }
    }
    bases.push(Base::Dormant {
      key: None,
      disabled: false,
    });
    bases.push(Base::Dormant {
      key: None,
      disabled: true,
    });
    for key in all_keys() {
      for disabled in [false, true] {
        bases.push(Base::Dormant {
          key: Some(key.clone()),
          disabled,
        });
      }
    }
    bases
  }

  fn all_locals() -> Vec<Local> {
    let mut locals = vec![Local::Absent];
    for key in all_keys() {
      for disabled in [false, true] {
        locals.push(Local::Present {
          key: key.clone(),
          disabled,
        });
      }
    }
    for (i, enabled) in all_keys().iter().enumerate() {
      for disabled_key in all_keys().iter().skip(i + 1) {
        locals.push(Local::Both {
          enabled: enabled.clone(),
          disabled: disabled_key.clone(),
        });
      }
    }
    locals
  }

  /// The §5.1/§5.3 tables and §5.2, spelled out once more as a second,
  /// independent reading. Every combination of policy, baseline, local state
  /// and the effectively-unchanged flag must agree with `decide`.
  fn oracle(path: &str, input: &DecisionInput) -> Decision {
    let (local_key, local_disabled) = local_parts(&input.local);
    let entry = |key: &FileKey, disabled: bool| managed(path, key, disabled);
    match input.policy {
      Policy::Ignore => match &input.base {
        Base::Managed { key, disabled } => decision(
          Action::Retire,
          None,
          dormant(path, Some(key.clone()), *disabled),
        ),
        _ => UNTOUCHED,
      },
      Policy::Delete => decision(
        if local_key.is_some() {
          Action::Delete
        } else {
          Action::Nothing
        },
        None,
        NextBaseline::Remove,
      ),
      Policy::Unmentioned => match &input.base {
        Base::Managed { key, .. } => match &local_key {
          None => decision(Action::Nothing, None, NextBaseline::Remove),
          Some(local) if local == key || input.effectively_unchanged => {
            decision(Action::Delete, None, NextBaseline::Remove)
          }
          Some(_) => decision(
            Action::Nothing,
            Some(DecisionConflict::ModifiedRemovedRemotely),
            NextBaseline::Remove,
          ),
        },
        _ => UNTOUCHED,
      },
      Policy::Ship => {
        let ship = input.remote.unwrap();
        if ship.once {
          return match &input.base {
            Base::Dormant { .. } => UNTOUCHED,
            Base::Managed { key, disabled } => decision(
              Action::Retire,
              None,
              dormant(path, Some(key.clone()), *disabled),
            ),
            Base::None => match &local_key {
              None => {
                let disabled = ship.disabled.unwrap_or(false);
                decision(
                  write_remote_key(&ship.key, disabled),
                  None,
                  dormant(path, Some(ship.key.clone()), disabled),
                )
              }
              Some(key) if *key == ship.key => decision(
                Action::Nothing,
                None,
                dormant(
                  path,
                  Some(ship.key.clone()),
                  local_disabled.unwrap_or(false),
                ),
              ),
              Some(_) => decision(Action::Nothing, None, dormant(path, None, false)),
            },
          };
        }
        let base_disabled = match &input.base {
          Base::Managed { disabled, .. } => Some(*disabled),
          Base::Dormant { key, disabled } => key.as_ref().map(|_| *disabled),
          Base::None => None,
        };
        let outcome = disabled_outcome(ship.disabled, base_disabled, local_disabled);
        let record = disabled_record(ship.disabled, local_disabled);
        let conflicted_record =
          disabled_record_conflicted(ship.disabled, base_disabled, local_disabled);
        let next = entry(&ship.key, record);
        let conflicted_next = entry(&ship.key, conflicted_record);
        match &input.base {
          Base::None | Base::Dormant { key: None, .. } => match &local_key {
            None => decision(write_remote_key(&ship.key, outcome), None, next),
            Some(key) if *key == ship.key => decision(
              rename_unless_there(key, outcome, local_disabled),
              None,
              next,
            ),
            Some(_) => decision(
              Action::Nothing,
              Some(DecisionConflict::UnmanagedFileInTheWay),
              conflicted_next,
            ),
          },
          Base::Dormant {
            key: Some(base_key),
            ..
          } => {
            let base_key = base_key.clone();
            match &local_key {
              None => decision(write_remote_key(&ship.key, outcome), None, next),
              Some(key) if *key == ship.key => decision(
                rename_unless_there(key, outcome, local_disabled),
                None,
                next,
              ),
              Some(key) if *key == base_key => {
                decision(write_remote_key(&ship.key, outcome), None, next)
              }
              Some(_) => decision(
                Action::Nothing,
                Some(DecisionConflict::Remanage),
                conflicted_next,
              ),
            }
          }
          Base::Managed { key: base_key, .. } => {
            let base_key = base_key.clone();
            match &local_key {
              None => decision(
                Action::Nothing,
                (ship.key != base_key).then_some(DecisionConflict::DeletedChangedRemotely),
                next,
              ),
              Some(key) => {
                if ship.key == base_key {
                  decision(
                    rename_unless_there(key, outcome, local_disabled),
                    None,
                    next,
                  )
                } else if *key == base_key {
                  decision(write_remote_key(&ship.key, outcome), None, next)
                } else if *key == ship.key {
                  decision(
                    rename_unless_there(key, outcome, local_disabled),
                    None,
                    next,
                  )
                } else {
                  decision(
                    Action::Nothing,
                    Some(DecisionConflict::ModifiedChangedRemotely),
                    next,
                  )
                }
              }
            }
          }
        }
      }
    }
  }

  fn write_remote_key(key: &FileKey, disabled: bool) -> Action {
    Action::WriteRemote {
      key: key.clone(),
      variant: Variant::of_disabled(disabled),
    }
  }

  fn rename_unless_there(key: &FileKey, outcome: bool, local_disabled: Option<bool>) -> Action {
    if local_disabled == Some(outcome) {
      Action::Nothing
    } else {
      Action::Rename {
        key: key.clone(),
        variant: Variant::of_disabled(outcome),
      }
    }
  }

  #[test]
  fn every_combination_matches_the_tables() {
    let path = "config/a.toml";
    let mut count = 0usize;
    for base in all_bases() {
      for local in all_locals() {
        for flag in [false, true] {
          // The non-ship policies carry no remote input.
          for policy in [Policy::Ignore, Policy::Delete, Policy::Unmentioned] {
            let input = DecisionInput {
              policy,
              remote: None,
              base: base.clone(),
              local: local.clone(),
              effectively_unchanged: flag,
            };
            let actual = decide(path, &input);
            assert_eq!(
              actual,
              oracle(path, &input),
              "policy={policy:?} base={base:?} local={local:?} flag={flag}"
            );
            count += 1;
          }
          for remote in all_ship_inputs() {
            let input = DecisionInput {
              policy: Policy::Ship,
              remote: Some(&remote),
              base: base.clone(),
              local: local.clone(),
              effectively_unchanged: flag,
            };
            let actual = decide(path, &input);
            assert_eq!(
              actual,
              oracle(path, &input),
              "remote={remote:?} base={base:?} local={local:?} flag={flag}"
            );
            count += 1;
          }
        }
      }
    }
    // Every combination answered exactly once, and the run really covered the
    // full product.
    assert!(count > 10_000);
  }
}
