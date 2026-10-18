//! Unified line diffs for showing changes, in the shape of `git diff`.

use super::merge::line_hunks;
use serde::Serialize;

/// Lines of unchanged text kept around each change.
pub const CONTEXT_LINES: usize = 3;

/// Above this many line pairs the middle of the file is shown as one
/// replacement instead of running the quadratic line matcher.
const MAX_MATCH_CELLS: usize = 4_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffLineKind {
  Context,
  Add,
  Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
  pub kind: DiffLineKind,
  /// 1-based line number on the old side; absent for added lines.
  pub old_number: Option<usize>,
  /// 1-based line number on the new side; absent for deleted lines.
  pub new_number: Option<usize>,
  pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffHunk {
  /// 1-based start and length on each side, as in `@@ -a,b +c,d @@`.
  pub old_start: usize,
  pub old_lines: usize,
  pub new_start: usize,
  pub new_lines: usize,
  pub lines: Vec<DiffLine>,
}

/// One change: `old[start..end]` becomes `new[new_start..new_end]`.
#[derive(Debug, Clone, Copy)]
struct Edit {
  start: usize,
  end: usize,
  new_start: usize,
  new_end: usize,
}

fn edits(old: &[String], new: &[String]) -> Vec<Edit> {
  // Trim what both sides share at the ends; most config edits are small.
  let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
  let suffix = old[prefix..]
    .iter()
    .rev()
    .zip(new[prefix..].iter().rev())
    .take_while(|(a, b)| a == b)
    .count();
  let old_mid = &old[prefix..old.len() - suffix];
  let new_mid = &new[prefix..new.len() - suffix];
  if old_mid.is_empty() && new_mid.is_empty() {
    return Vec::new();
  }
  if old_mid.len().saturating_mul(new_mid.len()) > MAX_MATCH_CELLS {
    return vec![Edit {
      start: prefix,
      end: prefix + old_mid.len(),
      new_start: prefix,
      new_end: prefix + new_mid.len(),
    }];
  }
  // Hunks are positioned on the old side; walk them to find the new side.
  let mut result = Vec::new();
  let mut shift: isize = 0;
  for hunk in line_hunks(old_mid, new_mid) {
    let start = prefix + hunk.base_start;
    let end = prefix + hunk.base_end;
    let new_start = (start as isize + shift) as usize;
    let new_end = new_start + hunk.replacement.len();
    shift += hunk.replacement.len() as isize - (end - start) as isize;
    result.push(Edit {
      start,
      end,
      new_start,
      new_end,
    });
  }
  result
}

/// Diff two texts already split into lines, grouping changes whose context
/// would touch into one hunk.
pub fn unified_diff(old: &[String], new: &[String], context: usize) -> Vec<DiffHunk> {
  let edits = edits(old, new);
  let mut groups: Vec<Vec<Edit>> = Vec::new();
  for edit in edits {
    match groups.last_mut() {
      Some(group) if edit.start <= group.last().unwrap().end + 2 * context => group.push(edit),
      _ => groups.push(vec![edit]),
    }
  }
  groups
    .into_iter()
    .map(|group| {
      let first = group[0];
      let last = *group.last().unwrap();
      let old_from = first.start.saturating_sub(context);
      let new_from = first.new_start - (first.start - old_from);
      let old_to = (last.end + context).min(old.len());
      let mut lines = Vec::new();
      let (mut i, mut j) = (old_from, new_from);
      let push_context = |i: &mut usize, j: &mut usize, until: usize, lines: &mut Vec<DiffLine>| {
        while *i < until {
          lines.push(DiffLine {
            kind: DiffLineKind::Context,
            old_number: Some(*i + 1),
            new_number: Some(*j + 1),
            text: old[*i].clone(),
          });
          *i += 1;
          *j += 1;
        }
      };
      for edit in &group {
        push_context(&mut i, &mut j, edit.start, &mut lines);
        for k in edit.start..edit.end {
          lines.push(DiffLine {
            kind: DiffLineKind::Delete,
            old_number: Some(k + 1),
            new_number: None,
            text: old[k].clone(),
          });
        }
        for k in edit.new_start..edit.new_end {
          lines.push(DiffLine {
            kind: DiffLineKind::Add,
            old_number: None,
            new_number: Some(k + 1),
            text: new[k].clone(),
          });
        }
        i = edit.end;
        j = edit.new_end;
      }
      push_context(&mut i, &mut j, old_to, &mut lines);
      let old_lines = old_to - old_from;
      let new_lines = j - new_from;
      DiffHunk {
        // `git diff` writes the line before an empty side as its start.
        old_start: if old_lines == 0 {
          old_from
        } else {
          old_from + 1
        },
        old_lines,
        new_start: if new_lines == 0 {
          new_from
        } else {
          new_from + 1
        },
        new_lines,
        lines,
      }
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn lines(value: &str) -> Vec<String> {
    value.lines().map(str::to_string).collect()
  }

  fn render(hunks: &[DiffHunk]) -> String {
    let mut out = String::new();
    for hunk in hunks {
      out += &format!(
        "@@ -{},{} +{},{} @@\n",
        hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
      );
      for line in &hunk.lines {
        let mark = match line.kind {
          DiffLineKind::Context => ' ',
          DiffLineKind::Add => '+',
          DiffLineKind::Delete => '-',
        };
        out += &format!("{mark}{}\n", line.text);
      }
    }
    out
  }

  #[test]
  fn identical_texts_have_no_hunks() {
    assert!(unified_diff(&lines("a\nb"), &lines("a\nb"), 3).is_empty());
  }

  #[test]
  fn a_change_carries_its_context_and_numbers() {
    let old = lines("1\n2\n3\n4\n5\n6\n7\n8\n9");
    let new = lines("1\n2\n3\n4\nfive\n6\n7\n8\n9");
    let hunks = unified_diff(&old, &new, 3);
    assert_eq!(
      render(&hunks),
      "@@ -2,7 +2,7 @@\n 2\n 3\n 4\n-5\n+five\n 6\n 7\n 8\n"
    );
    let changed = &hunks[0].lines[3];
    assert_eq!((changed.old_number, changed.new_number), (Some(5), None));
    let added = &hunks[0].lines[4];
    assert_eq!((added.old_number, added.new_number), (None, Some(5)));
  }

  #[test]
  fn distant_changes_are_separate_hunks_and_near_ones_join() {
    let old: Vec<String> = (1..=20).map(|n| n.to_string()).collect();
    let mut new = old.clone();
    new[1] = "two".into();
    new[18] = "nineteen".into();
    assert_eq!(unified_diff(&old, &new, 3).len(), 2);
    let mut near = old.clone();
    near[1] = "two".into();
    near[7] = "eight".into();
    assert_eq!(unified_diff(&old, &near, 3).len(), 1);
  }

  #[test]
  fn insertions_and_deletions_shift_new_numbers() {
    let old = lines("a\nb\nc\nd");
    let new = lines("a\nx\ny\nb\nd");
    let rendered = render(&unified_diff(&old, &new, 1));
    assert_eq!(rendered, "@@ -1,4 +1,5 @@\n a\n+x\n+y\n b\n-c\n d\n");
  }

  #[test]
  fn creating_a_file_starts_from_zero() {
    let hunks = unified_diff(&[], &lines("a\nb"), 3);
    assert_eq!(render(&hunks), "@@ -0,0 +1,2 @@\n+a\n+b\n");
  }
}
