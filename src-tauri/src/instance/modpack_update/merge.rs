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

struct EditGroup {
  start: usize,
  end: usize,
  local: Vec<Hunk>,
  remote: Vec<Hunk>,
}

impl EditGroup {
  fn conflicting(&self) -> bool {
    !self.local.is_empty() && !self.remote.is_empty() && self.local != self.remote
  }
}

/// Connected ranges form one answerable conflict, including adjacency.
fn edit_groups(base: &[String], local: &[String], remote: &[String]) -> Vec<EditGroup> {
  let mut edits: Vec<_> = hunks(base, local)
    .into_iter()
    .map(|h| (h, false))
    .chain(hunks(base, remote).into_iter().map(|h| (h, true)))
    .collect();
  edits.sort_by_key(|(h, _)| (h.base_start, h.base_end));
  let mut groups: Vec<EditGroup> = Vec::new();
  for (edit, remote) in edits {
    if !groups.last().is_some_and(|g| edit.base_start <= g.end) {
      groups.push(EditGroup {
        start: edit.base_start,
        end: edit.base_end,
        local: vec![],
        remote: vec![],
      });
    }
    let group = groups.last_mut().unwrap();
    group.end = group.end.max(edit.base_end);
    if remote {
      group.remote.push(edit);
    } else {
      group.local.push(edit);
    }
  }
  groups
}

fn region(base: &[String], edits: &[Hunk], start: usize, end: usize) -> Vec<String> {
  let relative: Vec<_> = edits
    .iter()
    .map(|h| Hunk {
      base_start: h.base_start - start,
      base_end: h.base_end - start,
      replacement: h.replacement.clone(),
    })
    .collect();
  apply_hunks(&base[start..end], &relative)
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
  let mut conflicts = Vec::new();
  let mut accepted = Vec::new();
  for group in edit_groups(base, local, remote) {
    if group.conflicting() {
      conflicts.push(MergeConflict {
        base_start: group.start,
        base_end: group.end,
        base: base[group.start..group.end].to_vec(),
        local: region(base, &group.local, group.start, group.end),
        remote: region(base, &group.remote, group.start, group.end),
      });
    } else {
      accepted.extend(if group.local.is_empty() {
        group.remote
      } else {
        group.local
      });
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
  let mut accepted = Vec::new();
  let mut conflict_index = 0usize;
  for group in edit_groups(base, local, remote) {
    let take_remote = if group.conflicting() {
      let selected = remote_hunks.contains(&conflict_index);
      conflict_index += 1;
      selected
    } else {
      group.local.is_empty()
    };
    accepted.extend(if take_remote {
      group.remote
    } else {
      group.local
    });
  }
  accepted.sort_by_key(|edit| (edit.base_start, edit.base_end));
  apply_hunks(base, &accepted)
}

pub fn merge_with_choices(
  path: &str,
  base: &[u8],
  local: &[u8],
  remote: &[u8],
  selected: &[usize],
) -> Result<Vec<u8>, TextMergeError> {
  let parse = |bytes: &[u8]| -> Result<Vec<String>, TextMergeError> {
    let text = std::str::from_utf8(bytes).map_err(|_| TextMergeError::InvalidText)?;
    if !valid_format(path, text) {
      return Err(TextMergeError::InvalidText);
    }
    Ok(
      text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::to_owned)
        .collect(),
    )
  };
  let merged = three_way_with_choices(&parse(base)?, &parse(local)?, &parse(remote)?, selected);
  let output = merged.join("\n") + "\n";
  if !valid_format(path, &output) {
    return Err(TextMergeError::InvalidMergedFormat);
  }
  Ok(output.replace('\n', line_ending(local)).into_bytes())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn lines(value: &str) -> Vec<String> {
    value.lines().map(str::to_string).collect()
  }

  #[test]
  fn bridged_edits_form_one_complete_conflict() {
    let base = lines("a\nb\nc\nd\ne\nf\ng");
    let local = lines("a\nX\nY\nZ\ne\nf\ng");
    let remote = lines("a\nB\nc\nD\ne\nf\nG");
    let result = three_way(&base, &local, &remote);
    assert_eq!(result.conflicts.len(), 1);
    assert_eq!(result.conflicts[0].remote, lines("B\nc\nD"));
    assert_eq!(
      three_way_with_choices(&base, &local, &remote, &[]),
      lines("a\nX\nY\nZ\ne\nf\nG")
    );
    assert_eq!(three_way_with_choices(&base, &local, &remote, &[0]), remote);
  }

  #[test]
  fn selected_merge_validates_the_combined_format() {
    // Each side is valid TOML, but combining their independent additions duplicates a key.
    let base = b"a=0\n# gap\nb=0\n";
    let local = b"a=0\nx=1\n# gap\nb=0\n";
    let remote = b"a=0\n# gap\nb=0\nx=2\n";
    assert_eq!(
      merge_with_choices("a.toml", base, local, remote, &[]),
      Err(TextMergeError::InvalidMergedFormat)
    );
    assert_eq!(
      merge_with_choices("a.txt", b"a\r\n", b"b\r\n", b"c\n", &[0]).unwrap(),
      b"c\r\n"
    );
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
