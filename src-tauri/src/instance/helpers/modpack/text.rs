//! Conservative text classification and normalization for modpack files.

use regex::Regex;
use std::path::Path;

pub const MAX_TEXT_SIZE: usize = 2 * 1024 * 1024;

const TEXT_EXTENSIONS: &[&str] = &["cfg", "conf", "ini", "json", "properties", "toml", "txt"];

fn extension(path: &str) -> Option<&str> {
  Path::new(path).extension().and_then(|value| value.to_str())
}

/// Whether a file is eligible for the conservative, line-based text path.
pub fn is_text_path(path: &str, bytes: &[u8]) -> bool {
  bytes.len() <= MAX_TEXT_SIZE
    && extension(path)
      .map(|value| {
        TEXT_EXTENSIONS
          .iter()
          .any(|known| value.eq_ignore_ascii_case(known))
      })
      .unwrap_or(false)
    && !bytes.contains(&0)
    && std::str::from_utf8(bytes).is_ok()
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
  fn rejects_unknown_binary_and_large_files() {
    assert!(!is_text_path("config/a.yaml", b"a: b"));
    assert!(!is_text_path("config/a.toml", b"a\0b"));
    assert!(!is_text_path(
      "config/a.toml",
      &vec![b'a'; MAX_TEXT_SIZE + 1]
    ));
  }
}
