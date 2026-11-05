//! Conservative text classification and normalization for modpack files.

use regex::Regex;
use serde::Serialize;
use std::path::Path;

pub const MAX_TEXT_SIZE: usize = 2 * 1024 * 1024;

const TEXT_EXTENSIONS: &[&str] = &[
  "cfg",
  "conf",
  "ini",
  "json",
  "lang",
  "properties",
  "toml",
  "txt",
];

fn extension(path: &str) -> Option<&str> {
  Path::new(path).extension().and_then(|value| value.to_str())
}

/// Why a file is not eligible for the line-based text path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NotTextReason {
  /// The extension is not one of the text types.
  NotTextType,
  /// Larger than `MAX_TEXT_SIZE`.
  TooLarge,
  /// Contains a NUL byte.
  Binary,
  /// Not valid UTF-8.
  NotUtf8,
}

/// The reasons that need no content: the type, then the size.
pub fn size_reason(path: &str, size: u64) -> Result<(), NotTextReason> {
  let known = extension(path).is_some_and(|value| {
    TEXT_EXTENSIONS
      .iter()
      .any(|known| value.eq_ignore_ascii_case(known))
  });
  if !known {
    Err(NotTextReason::NotTextType)
  } else if size > MAX_TEXT_SIZE as u64 {
    Err(NotTextReason::TooLarge)
  } else {
    Ok(())
  }
}

/// Why a file is not text, checked in this order: type, size, NUL byte,
/// UTF-8. The first failing check names the reason.
pub fn text_reason(path: &str, bytes: &[u8]) -> Result<(), NotTextReason> {
  size_reason(path, bytes.len() as u64)?;
  if bytes.contains(&0) {
    Err(NotTextReason::Binary)
  } else if std::str::from_utf8(bytes).is_err() {
    Err(NotTextReason::NotUtf8)
  } else {
    Ok(())
  }
}

/// Whether a file is eligible for the conservative, line-based text path.
pub fn is_text_path(path: &str, bytes: &[u8]) -> bool {
  text_reason(path, bytes).is_ok()
}

fn normalize_properties_header(text: &str) -> String {
  let timestamp = Regex::new(
    r"^#[A-Z][a-z]{2} [A-Z][a-z]{2} [0-9]{1,2} [0-9]{2}:[0-9]{2}:[0-9]{2} [A-Z]{2,5} [0-9]{4}$",
  )
  .expect("properties timestamp regex is valid");
  let mut lines = text.lines();
  let mut output = Vec::new();
  if let Some(first) = lines.next() {
    if !timestamp.is_match(first) {
      output.push(first);
    }
  }
  output.extend(lines);
  output.join("\n")
}

/// Return normalized UTF-8 text. The input is not modified on disk.
pub fn normalize_text(path: &str, bytes: &[u8]) -> Option<String> {
  if !is_text_path(path, bytes) {
    return None;
  }
  let text = std::str::from_utf8(bytes)
    .expect("is_text_path validated UTF-8")
    .strip_prefix('\u{feff}')
    .unwrap_or_else(|| std::str::from_utf8(bytes).expect("validated UTF-8"))
    .replace("\r\n", "\n")
    .replace('\r', "\n");
  let text = if path
    .rsplit_once('.')
    .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("properties"))
  {
    normalize_properties_header(&text)
  } else {
    text
  };
  Some(if text.ends_with('\n') {
    text
  } else {
    format!("{text}\n")
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn normalizes_line_endings_bom_and_final_newline() {
    assert_eq!(
      normalize_text("config/a.toml", b"\xEF\xBB\xBFa=1\r\nb=2"),
      Some("a=1\nb=2\n".into())
    );
    assert_eq!(
      normalize_text("config/a.txt", b"a\rb\n"),
      Some("a\nb\n".into())
    );
  }

  #[test]
  fn removes_only_a_properties_timestamp_header() {
    assert_eq!(
      normalize_text(
        "config/a.properties",
        b"#Wed Oct 14 20:14:57 CST 2026\r\na=b"
      ),
      Some("a=b\n".into())
    );
    assert_eq!(
      normalize_text("config/a.properties", b"# user note\r\na=b"),
      Some("# user note\na=b\n".into())
    );
  }

  #[test]
  fn lang_files_are_text() {
    assert!(is_text_path("assets/mod/lang/en_US.lang", b"a.b=c\n"));
    assert!(is_text_path(
      "assets/mod/lang/zh_CN.LANG",
      "a=你好\n".as_bytes()
    ));
    assert_eq!(
      normalize_text("assets/mod/lang/en_US.lang", b"a=1\r\nb=2"),
      Some("a=1\nb=2\n".into())
    );
    // Only UTF-8 qualifies: a Latin-1 file stays a whole-file choice.
    assert!(!is_text_path("assets/mod/lang/de_DE.lang", b"a=\xE4\n"));
  }

  #[test]
  fn rejects_unknown_binary_and_large_files() {
    assert!(!is_text_path("config/a.yaml", b"a: b"));
    assert!(!is_text_path("config/a.toml", b"a\0b"));
    assert!(!is_text_path(
      "config/a.toml",
      &vec![b'a'; MAX_TEXT_SIZE + 1]
    ));
  }

  #[test]
  fn each_failure_names_its_reason() {
    assert_eq!(text_reason("config/a.toml", b"a=1"), Ok(()));
    assert_eq!(
      text_reason("mods/a.jar", b"PK"),
      Err(NotTextReason::NotTextType)
    );
    assert_eq!(
      text_reason("config/a.toml", &vec![b'a'; MAX_TEXT_SIZE + 1]),
      Err(NotTextReason::TooLarge)
    );
    assert_eq!(
      text_reason("config/a.toml", &vec![b'a'; MAX_TEXT_SIZE]),
      Ok(())
    );
    assert_eq!(
      text_reason("config/a.toml", b"a\0b"),
      Err(NotTextReason::Binary)
    );
    assert_eq!(
      text_reason("config/a.toml", b"a=\xE4"),
      Err(NotTextReason::NotUtf8)
    );
  }

  #[test]
  fn the_first_failing_check_wins() {
    let big = vec![0u8; MAX_TEXT_SIZE + 1];
    // Type before size, size before content, NUL before encoding.
    assert_eq!(
      text_reason("mods/a.jar", &big),
      Err(NotTextReason::NotTextType)
    );
    assert_eq!(
      text_reason("config/a.toml", &big),
      Err(NotTextReason::TooLarge)
    );
    assert_eq!(
      text_reason("mods/a.jar", b"\0\xFF"),
      Err(NotTextReason::NotTextType)
    );
    assert_eq!(
      text_reason("config/a.toml", b"\0\xFF"),
      Err(NotTextReason::Binary)
    );
  }

  #[test]
  fn the_size_check_alone_needs_no_content() {
    assert_eq!(size_reason("config/a.toml", 10), Ok(()));
    assert_eq!(
      size_reason("config/a.toml", MAX_TEXT_SIZE as u64 + 1),
      Err(NotTextReason::TooLarge)
    );
    assert_eq!(size_reason("a.png", 1), Err(NotTextReason::NotTextType));
  }

  #[test]
  fn reasons_serialize_as_the_names_the_frontend_reads() {
    let names: Vec<_> = [
      NotTextReason::NotTextType,
      NotTextReason::TooLarge,
      NotTextReason::Binary,
      NotTextReason::NotUtf8,
    ]
    .iter()
    .map(|reason| serde_json::to_string(reason).unwrap())
    .collect();
    assert_eq!(
      names,
      vec![
        "\"notTextType\"",
        "\"tooLarge\"",
        "\"binary\"",
        "\"notUtf8\""
      ]
    );
  }
}
