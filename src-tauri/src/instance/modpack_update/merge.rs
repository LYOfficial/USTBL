//! Conservative line-hunk three-way merging for eligible configuration text.

#![allow(dead_code)]

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
  pub base_start: usize,
  pub base_end: usize,
  pub replacement: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeConflict {
  pub base_start: usize,
  pub base_end: usize,
  /// `base[base_start..base_end]`, the lines both sides changed.
  pub base: Vec<String>,
  /// The same range as each side has it.
  pub local: Vec<String>,
  pub remote: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeResult {
  pub lines: Vec<String>,
  pub conflicts: Vec<MergeConflict>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextMergeError {
  InvalidText,
  InvalidMergedFormat,
  Conflict(MergeResult),
}

/// Return the line-level conflicts while keeping the local file as the
/// fallback result. The caller may replace selected hunks with remote lines.
pub fn text_conflicts(
  path: &str,
  base: &[u8],
  local: &[u8],
  remote: &[u8],
) -> Result<Vec<MergeConflict>, TextMergeError> {
  let to_lines = |bytes: &[u8]| {
    std::str::from_utf8(bytes)
      .map(|text| text.replace("\r\n", "\n").replace('\r', "\n"))
      .map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
  };
  let (base, local, remote) = (to_lines(base), to_lines(local), to_lines(remote));
  let (Ok(base), Ok(local), Ok(remote)) = (base, local, remote) else {
    return Err(TextMergeError::InvalidText);
  };
  if !valid_format(path, &base.join("\n"))
    || !valid_format(path, &local.join("\n"))
    || !valid_format(path, &remote.join("\n"))
  {
    return Err(TextMergeError::InvalidText);
  }
  Ok(three_way(&base, &local, &remote).conflicts)
}

fn line_ending(bytes: &[u8]) -> &'static str {
  let crlf = bytes.windows(2).filter(|pair| *pair == b"\r\n").count();
  let lf = bytes.iter().filter(|byte| **byte == b'\n').count();
  if crlf * 2 >= lf { "\r\n" } else { "\n" }
}

fn valid_format(path: &str, text: &str) -> bool {
  let Some(extension) = path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()) else {
    return true;
  };
  match extension.as_str() {
    "json" => serde_json::from_str::<serde_json::Value>(text).is_ok(),
    "toml" => text.parse::<toml::Value>().is_ok(),
    "cfg" => {
      let mut balance = 0i32;
      for byte in text.bytes() {
        match byte {
          b'{' => balance += 1,
          b'}' => {
            balance -= 1;
            if balance < 0 {
              return false;
            }
          }
          _ => {}
        }
      }
      balance == 0
    }
    _ => true,
  }
}

/// Merge three eligible text files and preserve the local file's line ending style.
pub fn merge_text(path: &str, base: &[u8], local: &[u8], remote: &[u8]) -> Result<Vec<u8>, TextMergeError> {
  let to_lines = |bytes: &[u8]| {
    std::str::from_utf8(bytes)
      .map(|text| text.replace("\r\n", "\n").replace('\r', "\n"))
      .map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
  };
  let (base_text, local_text, remote_text) = (
    to_lines(base),
    to_lines(local),
    to_lines(remote),
  );
  let (Ok(base_text), Ok(local_text), Ok(remote_text)) = (base_text, local_text, remote_text) else {
    return Err(TextMergeError::InvalidText);
  };
  if !valid_format(path, &base_text.join("\n"))
    || !valid_format(path, &local_text.join("\n"))
    || !valid_format(path, &remote_text.join("\n"))
  {
    return Err(TextMergeError::InvalidText);
  }
  let merged = three_way(&base_text, &local_text, &remote_text);
  if !merged.conflicts.is_empty() {
    return Err(TextMergeError::Conflict(merged));
  }
  let normalized = merged.lines.join("\n") + "\n";
  if !valid_format(path, &normalized) {
    return Err(TextMergeError::InvalidMergedFormat);
  }
  Ok(normalized.replace('\n', line_ending(local)).into_bytes())
}

/// The changes that turn `base` into `changed`, positioned on `base`.
pub fn line_hunks(base: &[String], changed: &[String]) -> Vec<Hunk> {
  hunks(base, changed)
}

fn hunks(base: &[String], changed: &[String]) -> Vec<Hunk> {
  let n = base.len();
  let m = changed.len();
  let mut lcs = vec![vec![0usize; m + 1]; n + 1];
  for i in (0..n).rev() {
    for j in (0..m).rev() {
      lcs[i][j] = if base[i] == changed[j] {
        lcs[i + 1][j + 1] + 1
      } else {
        lcs[i + 1][j].max(lcs[i][j + 1])
      };
    }
  }
  let mut edits = Vec::new();
  let (mut i, mut j) = (0, 0);
  while i < n || j < m {
    if i < n && j < m && base[i] == changed[j] {
      i += 1;
      j += 1;
    } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
      edits.push((i, i, vec![changed[j].clone()]));
      j += 1;
    } else {
      edits.push((i, i + 1, Vec::new()));
      i += 1;
    }
  }
  let mut result = Vec::new();
  for (start, end, replacement) in edits {
    if let Some(last) = result.last_mut() {
      let last: &mut Hunk = last;
      if last.base_end == start {
        last.base_end = end;
        last.replacement.extend(replacement);
        continue;
      }
    }
    if start != end || !replacement.is_empty() {
      result.push(Hunk { base_start: start, base_end: end, replacement });
    }
  }
  result
}

fn overlaps_or_touches(a: &Hunk, b: &Hunk) -> bool {
  a.base_start <= b.base_end && b.base_start <= a.base_end
    || a.base_end == b.base_start
    || b.base_end == a.base_start
}

/// What `base[start..end]` reads as once `edit`, which lies inside it, is applied.
fn region(base: &[String], edit: &Hunk, start: usize, end: usize) -> Vec<String> {
  let mut lines = base[start..edit.base_start].to_vec();
  lines.extend(edit.replacement.iter().cloned());
  lines.extend_from_slice(&base[edit.base_end..end]);
  lines
}

fn apply_hunks(base: &[String], edits: &[Hunk]) -> Vec<String> {
  let mut output = Vec::new();
  let mut cursor = 0;
  for edit in edits {
    output.extend_from_slice(&base[cursor..edit.base_start]);
    output.extend(edit.replacement.clone());
    cursor = edit.base_end;
  }
  output.extend_from_slice(&base[cursor..]);
  output
}

/// Merge local and remote edits. Adjacent edits are intentionally conflicts.
pub fn three_way(base: &[String], local: &[String], remote: &[String]) -> MergeResult {
  let local_hunks = hunks(base, local);
  let remote_hunks = hunks(base, remote);
  let mut conflicts = Vec::new();
  let mut accepted = Vec::new();
  for local_edit in &local_hunks {
    for remote_edit in &remote_hunks {
      if overlaps_or_touches(local_edit, remote_edit) {
        if local_edit == remote_edit {
          accepted.push(local_edit.clone());
        } else {
          let start = local_edit.base_start.min(remote_edit.base_start);
          let end = local_edit.base_end.max(remote_edit.base_end);
          conflicts.push(MergeConflict {
            base_start: start,
            base_end: end,
            base: base[start..end].to_vec(),
            local: region(base, local_edit, start, end),
            remote: region(base, remote_edit, start, end),
          });
        }
      }
    }
  }
  for edit in local_hunks.iter().chain(remote_hunks.iter()) {
    let touched = conflicts.iter().any(|conflict| {
      edit.base_start >= conflict.base_start && edit.base_end <= conflict.base_end
    });
    let duplicate = accepted.iter().any(|known: &Hunk| known == edit);
    if !touched && !duplicate {
      accepted.push(edit.clone());
    }
  }
  accepted.sort_by_key(|edit| (edit.base_start, edit.base_end));
  MergeResult { lines: apply_hunks(base, &accepted), conflicts }
}

/// Merge while replacing only the selected conflict hunks with the remote
/// side. Unselected hunks keep the local side.
pub fn three_way_with_choices(
  base: &[String],
  local: &[String],
  remote: &[String],
  remote_hunks: &[usize],
) -> Vec<String> {
  let local_hunks = hunks(base, local);
  let remote_hunks_all = hunks(base, remote);
  let mut accepted = Vec::new();
  let mut conflict_index = 0usize;
  for local_edit in &local_hunks {
    for remote_edit in &remote_hunks_all {
      if overlaps_or_touches(local_edit, remote_edit) && local_edit != remote_edit {
        if remote_hunks.contains(&conflict_index) {
          accepted.push(remote_edit.clone());
        } else {
          accepted.push(local_edit.clone());
        }
        conflict_index += 1;
      }
    }
  }
  for edit in local_hunks.iter().chain(remote_hunks_all.iter()) {
    if !accepted.iter().any(|known: &Hunk| known == edit) {
      let conflicting = accepted.iter().any(|known| overlaps_or_touches(known, edit));
      if !conflicting {
        accepted.push(edit.clone());
      }
    }
  }
  accepted.sort_by_key(|edit| (edit.base_start, edit.base_end));
  apply_hunks(base, &accepted)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn lines(value: &str) -> Vec<String> {
    value.lines().map(str::to_string).collect()
  }

  #[test]
  fn non_overlapping_changes_are_combined() {
    let result = three_way(&lines("a\nb\nc\nd"), &lines("A\nb\nc\nd"), &lines("a\nb\nC\nd"));
    assert!(result.conflicts.is_empty());
    assert_eq!(result.lines, lines("A\nb\nC\nd"));
  }

  #[test]
  fn overlapping_and_adjacent_changes_conflict() {
    let base = lines("a\nb\nc\nd");
    assert_eq!(three_way(&base, &lines("a\nB\nc\nd"), &lines("a\nX\nc\nd")).conflicts.len(), 1);
    assert_eq!(three_way(&base, &lines("a\nB\nc\nd"), &lines("a\nb\nC\nd")).conflicts.len(), 1);
  }

  #[test]
  fn a_conflict_carries_only_its_own_range() {
    let base = lines("a\nb\nc\nd");
    let result = three_way(&base, &lines("a\nB\nc\nd"), &lines("a\nb\nC\nd"));
    let conflict = &result.conflicts[0];
    assert_eq!((conflict.base_start, conflict.base_end), (1, 3));
    assert_eq!(conflict.base, lines("b\nc"));
    assert_eq!(conflict.local, lines("B\nc"));
    assert_eq!(conflict.remote, lines("b\nC"));
  }

  #[test]
  fn identical_edits_are_not_conflicts() {
    let result = three_way(&lines("a\nb"), &lines("a\nB"), &lines("a\nB"));
    assert!(result.conflicts.is_empty());
    assert_eq!(result.lines, lines("a\nB"));
  }

  #[test]
  fn insertions_at_different_positions_merge() {
    let result = three_way(&lines("a\nc"), &lines("a\nb\nc"), &lines("a\nx\nc"));
    assert_eq!(result.conflicts.len(), 1);
  }

  #[test]
  fn merge_text_preserves_local_line_endings_and_validates_json() {
    let merged = merge_text(
      "config/a.json",
      b"{\r\n  \"a\": 1,\r\n  \"gap\": 0,\r\n  \"b\": 1\r\n}\r\n",
      b"{\r\n  \"a\": 2,\r\n  \"gap\": 0,\r\n  \"b\": 1\r\n}\r\n",
      b"{\n  \"a\": 1,\n  \"gap\": 0,\n  \"b\": 2\n}\n",
    )
    .unwrap();
    assert!(merged.windows(2).any(|pair| pair == b"\r\n"));
    assert!(serde_json::from_slice::<serde_json::Value>(&merged).is_ok());
  }
}
