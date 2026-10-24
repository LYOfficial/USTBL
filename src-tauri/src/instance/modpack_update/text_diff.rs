//! Unified line diffs for showing changes, in the shape of `git diff`.

use super::merge::line_hunks;
use serde::Serialize;

/// Lines of unchanged text kept around each change.
pub const CONTEXT_LINES: usize = 3;

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
  /// Which of the hunk's `edits` a deleted or added line belongs to.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub edit: Option<usize>,
}

/// The old-side range of one change, 0-based: `old[start..end]`. An insertion
/// has `start == end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffEdit {
  pub start: usize,
  pub end: usize,
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
  /// The changes this hunk shows, in order.
  pub edits: Vec<DiffEdit>,
}

/// One change: `old[start..end]` becomes `new[new_start..new_end]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit {
  pub start: usize,
  pub end: usize,
  pub new_start: usize,
  pub new_end: usize,
}

/// The lines of a text as the diff and the local ignores count them.
pub fn lines_of(text: &str) -> Vec<String> {
  text.lines().map(str::to_owned).collect()
}

/// The changes that turn `old` into `new`, positioned on both sides.
pub fn edits(old: &[String], new: &[String]) -> Vec<Edit> {
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
#[cfg(test)]
pub fn unified_diff(old: &[String], new: &[String], context: usize) -> Vec<DiffHunk> {
  unified_diff_hiding(old, new, context, &|_| false)
}

/// Like `unified_diff`, but changes for which `hidden` is true are left out.
/// A hidden change splits hunks and cuts the context next to it short, so the
/// shown line numbers stay true.
pub fn unified_diff_hiding(
  old: &[String],
  new: &[String],
  context: usize,
  hidden: &dyn Fn(&Edit) -> bool,
) -> Vec<DiffHunk> {
  let all = edits(old, new);
  // (edit, previous edit end, next edit start) with hidden ones kept as walls.
  let mut groups: Vec<Vec<usize>> = Vec::new();
  let mut last_visible: Option<usize> = None;
  for (index, edit) in all.iter().enumerate() {
    if hidden(edit) {
      last_visible = None;
      continue;
    }
    match (groups.last_mut(), last_visible) {
      (Some(group), Some(prev))
        if prev == index.wrapping_sub(1) && edit.start <= all[prev].end + 2 * context =>
      {
        group.push(index)
      }
      _ => groups.push(vec![index]),
    }
    last_visible = Some(index);
  }
  groups
    .into_iter()
    .map(|group| {
      let first = all[group[0]];
      let last = all[*group.last().unwrap()];
      let before_wall = if group[0] == 0 {
        0
      } else {
        all[group[0] - 1].end
      };
      let after_wall = all
        .get(group.last().unwrap() + 1)
        .map_or(old.len(), |next| next.start);
      let old_from = first.start.saturating_sub(context).max(before_wall);
      let new_from = first.new_start - (first.start - old_from);
      let old_to = (last.end + context).min(after_wall);
      let mut lines = Vec::new();
      let mut edits_shown = Vec::new();
      let (mut i, mut j) = (old_from, new_from);
      let push_context = |i: &mut usize, j: &mut usize, until: usize, lines: &mut Vec<DiffLine>| {
        while *i < until {
          lines.push(DiffLine {
            kind: DiffLineKind::Context,
            old_number: Some(*i + 1),
            new_number: Some(*j + 1),
            text: old[*i].clone(),
            edit: None,
          });
          *i += 1;
          *j += 1;
        }
      };
      for (n, &index) in group.iter().enumerate() {
        let edit = all[index];
        push_context(&mut i, &mut j, edit.start, &mut lines);
        edits_shown.push(DiffEdit {
          start: edit.start,
          end: edit.end,
        });
        for k in edit.start..edit.end {
          lines.push(DiffLine {
            kind: DiffLineKind::Delete,
            old_number: Some(k + 1),
            new_number: None,
            text: old[k].clone(),
            edit: Some(n),
          });
        }
        for k in edit.new_start..edit.new_end {
          lines.push(DiffLine {
            kind: DiffLineKind::Add,
            old_number: None,
            new_number: Some(k + 1),
            text: new[k].clone(),
            edit: Some(n),
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
        edits: edits_shown,
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

  #[test]
  fn hidden_edits_split_hunks_and_cut_context() {
    let old = lines("1\n2\n3\n4\n5\n6\n7");
    let new = lines("1\ntwo\n3\n4\n5\nsix\n7");
    let hunks = unified_diff_hiding(&old, &new, 3, &|e| e.start == 1);
    assert_eq!(hunks.len(), 1);
    assert_eq!(
      render(&hunks),
      "@@ -3,5 +3,5 @@\n 3\n 4\n 5\n-6\n+six\n 7\n"
    );
    assert_eq!(hunks[0].edits, vec![DiffEdit { start: 5, end: 6 }]);
    assert_eq!(hunks[0].lines[3].edit, Some(0));
    let both = unified_diff_hiding(&old, &new, 3, &|_| true);
    assert!(both.is_empty());
  }

  #[test]
  fn a_hidden_edit_between_two_shown_ones_keeps_them_apart() {
    let old = lines("a\nb\nc\nd\ne");
    let new = lines("A\nb\nC\nd\nE");
    let hunks = unified_diff_hiding(&old, &new, 3, &|e| e.start == 2);
    assert_eq!(hunks.len(), 2);
    assert_eq!(hunks[0].edits, vec![DiffEdit { start: 0, end: 1 }]);
    assert_eq!(hunks[1].edits, vec![DiffEdit { start: 4, end: 5 }]);
  }
}
