//! File errors that name the file they happened on.
//!
//! The modpack flows (first install, update and undo, publish) pass their
//! errors around as plain strings: command results, install records, task
//! card reasons and result events all carry a `USTBLError`. A file error is
//! therefore a string too, in the form
//!
//! ```text
//! <CODE>:{"file":"<path>","detail":"<system error>"}
//! ```
//!
//! `CODE` is the category (one of the `InstanceError` codes below), `file` the
//! path to show and `detail` the original system error. The frontend parses the
//! same text (`src/utils/file-error.ts`) and shows a sentence per category.
//!
//! The text is produced by [`FileFault`]'s `Display`. A `FileFault` rides
//! inside an `io::Error` (same `ErrorKind`), so the usual `?` into a
//! `USTBLError` yields the text without further changes at the conversion, and
//! callers can keep matching on `error.kind()`.

use crate::instance::models::misc::InstanceError;
use crate::instance::modpack_update::remote::UPDATE_DIR;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileErrorKind {
  /// Windows error 225/226: the file was blocked or removed by a virus scanner.
  Antivirus,
  /// Windows error 2/3.
  Missing,
  /// Windows error 5/32/33: in use by another program, or no permission.
  Inaccessible,
  /// Windows error 112/39.
  DiskFull,
  Other,
}

impl FileErrorKind {
  pub fn code(self) -> InstanceError {
    match self {
      FileErrorKind::Antivirus => InstanceError::FileBlockedByAntivirus,
      FileErrorKind::Missing => InstanceError::FileMissing,
      FileErrorKind::Inaccessible => InstanceError::FileInaccessible,
      FileErrorKind::DiskFull => InstanceError::DiskFull,
      FileErrorKind::Other => InstanceError::FileIoFailed,
    }
  }

  fn of_code(code: &str) -> Option<Self> {
    [
      FileErrorKind::Antivirus,
      FileErrorKind::Missing,
      FileErrorKind::Inaccessible,
      FileErrorKind::DiskFull,
      FileErrorKind::Other,
    ]
    .into_iter()
    .find(|kind| kind.code().to_string() == code)
  }
}

/// Windows system error codes by category.
#[cfg(windows)]
fn classify_os_code(code: i32) -> Option<FileErrorKind> {
  match code {
    // ERROR_VIRUS_INFECTED, ERROR_VIRUS_DELETED
    225 | 226 => Some(FileErrorKind::Antivirus),
    // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND
    2 | 3 => Some(FileErrorKind::Missing),
    // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
    5 | 32 | 33 => Some(FileErrorKind::Inaccessible),
    // ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL
    112 | 39 => Some(FileErrorKind::DiskFull),
    _ => None,
  }
}

#[cfg(not(windows))]
fn classify_os_code(_: i32) -> Option<FileErrorKind> {
  None
}

fn classify_kind(kind: io::ErrorKind) -> FileErrorKind {
  match kind {
    io::ErrorKind::NotFound => FileErrorKind::Missing,
    io::ErrorKind::PermissionDenied
    | io::ErrorKind::ResourceBusy
    | io::ErrorKind::ExecutableFileBusy
    | io::ErrorKind::ReadOnlyFilesystem => FileErrorKind::Inaccessible,
    io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => FileErrorKind::DiskFull,
    _ => FileErrorKind::Other,
  }
}

/// The category of an I/O error: by the Windows error code when there is one,
/// by `ErrorKind` otherwise.
pub fn classify(error: &io::Error) -> FileErrorKind {
  error
    .raw_os_error()
    .and_then(classify_os_code)
    .unwrap_or_else(|| classify_kind(error.kind()))
}

#[derive(Serialize, Deserialize)]
struct Payload {
  file: String,
  detail: String,
}

/// An I/O error together with the file it concerns.
#[derive(Debug)]
pub struct FileFault {
  kind: FileErrorKind,
  path: PathBuf,
  /// Paths inside this directory are shown relative to it.
  root: Option<PathBuf>,
  detail: String,
}

impl FileFault {
  fn file(&self) -> String {
    display_path(&self.path, self.root.as_deref())
  }
}

impl fmt::Display for FileFault {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let payload = Payload {
      file: self.file(),
      detail: self.detail.clone(),
    };
    match serde_json::to_string(&payload) {
      Ok(json) => write!(f, "{}:{json}", self.kind.code()),
      Err(_) => write!(f, "{}", self.kind.code()),
    }
  }
}

impl std::error::Error for FileFault {}

/// A parsed file error text.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct ParsedFileError {
  pub kind: FileErrorKind,
  pub file: String,
  pub detail: String,
}

/// Read a file error text back. Anything else, including the plain strings
/// older records hold, answers `None`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse(text: &str) -> Option<ParsedFileError> {
  let (code, rest) = text.split_once(":{")?;
  let kind = FileErrorKind::of_code(code)?;
  let json = format!("{{{rest}");
  let payload = serde_json::Deserializer::from_str(&json)
    .into_iter::<Payload>()
    .next()?
    .ok()?;
  Some(ParsedFileError {
    kind,
    file: payload.file,
    detail: payload.detail,
  })
}

fn native_separators(path: &str) -> String {
  path.replace(['/', '\\'], std::path::MAIN_SEPARATOR_STR)
}

/// How a path is shown: relative to `root` when it lies inside, from the
/// update folder on for the staging area (also when no root is known), the
/// absolute path otherwise.
pub fn display_path(path: &Path, root: Option<&Path>) -> String {
  if let Some(relative) = root.and_then(|root| path.strip_prefix(root).ok()) {
    return native_separators(&relative.to_string_lossy());
  }
  let components: Vec<_> = path.components().collect();
  if let Some(start) = components
    .iter()
    .position(|component| component.as_os_str() == UPDATE_DIR)
  {
    let relative: PathBuf = components[start..].iter().collect();
    return native_separators(&relative.to_string_lossy());
  }
  let text = path.to_string_lossy();
  text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

/// Attach `path` to `error`. An error that already carries a path keeps it
/// (the place closest to the cause knows best); it only learns `root` when it
/// had none.
pub fn tag(error: io::Error, path: &Path, root: Option<&Path>) -> io::Error {
  let kind = error.kind();
  if error.get_ref().is_some_and(|inner| inner.is::<FileFault>()) {
    let Some(inner) = error.into_inner() else {
      return io::Error::from(kind);
    };
    return match inner.downcast::<FileFault>() {
      Ok(mut fault) => {
        if fault.root.is_none() {
          fault.root = root.map(Path::to_path_buf);
        }
        io::Error::new(kind, *fault)
      }
      Err(other) => io::Error::new(kind, other),
    };
  }
  // Errors of our own checks (bad JSON, bad arguments) are not about the
  // file system; their text stays as it is.
  if matches!(
    kind,
    io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput
  ) {
    return error;
  }
  io::Error::new(
    kind,
    FileFault {
      kind: classify(&error),
      path: path.to_path_buf(),
      root: root.map(Path::to_path_buf),
      detail: error.to_string(),
    },
  )
}

pub trait AtPath<T> {
  /// Name `path` as the file the failed operation concerned.
  fn at(self, path: &Path) -> io::Result<T>;
  /// Like [`AtPath::at`], showing the path relative to `root` when inside it.
  fn at_in(self, path: &Path, root: &Path) -> io::Result<T>;
  /// Complete an error that already names a file with the directory it is
  /// shown relative to. Other errors pass through.
  fn rooted(self, root: &Path) -> io::Result<T>;
}

impl<T> AtPath<T> for io::Result<T> {
  fn at(self, path: &Path) -> io::Result<T> {
    self.map_err(|error| tag(error, path, None))
  }

  fn at_in(self, path: &Path, root: &Path) -> io::Result<T> {
    self.map_err(|error| tag(error, path, Some(root)))
  }

  fn rooted(self, root: &Path) -> io::Result<T> {
    self.map_err(|error| {
      if error.get_ref().is_some_and(|inner| inner.is::<FileFault>()) {
        tag(error, Path::new(""), Some(root))
      } else {
        error
      }
    })
  }
}

/// The error text for a failure that only arrived as text, such as the final
/// message of a download task. When it ends in the system error code of one of
/// the known file problems the text becomes a file error for `path`; anything
/// else, network errors included, is returned unchanged.
pub fn from_os_text(message: &str, path: &Path, root: Option<&Path>) -> String {
  let code = message
    .rsplit_once("(os error ")
    .and_then(|(_, tail)| tail.split_once(')'))
    .and_then(|(code, _)| code.trim().parse::<i32>().ok());
  let Some(code) = code else {
    return message.to_string();
  };
  let kind = classify(&io::Error::from_raw_os_error(code));
  if kind == FileErrorKind::Other {
    return message.to_string();
  }
  FileFault {
    kind,
    path: path.to_path_buf(),
    root: root.map(Path::to_path_buf),
    detail: message.to_string(),
  }
  .to_string()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(windows)]
  #[test]
  fn windows_codes_fall_into_the_five_categories() {
    let cases = [
      (225, FileErrorKind::Antivirus),
      (226, FileErrorKind::Antivirus),
      (2, FileErrorKind::Missing),
      (3, FileErrorKind::Missing),
      (5, FileErrorKind::Inaccessible),
      (32, FileErrorKind::Inaccessible),
      (33, FileErrorKind::Inaccessible),
      (112, FileErrorKind::DiskFull),
      (39, FileErrorKind::DiskFull),
      (1117, FileErrorKind::Other),
    ];
    for (code, expected) in cases {
      assert_eq!(
        classify(&io::Error::from_raw_os_error(code)),
        expected,
        "os error {code}"
      );
    }
  }

  #[test]
  fn without_an_os_code_the_error_kind_decides() {
    let cases = [
      (io::ErrorKind::NotFound, FileErrorKind::Missing),
      (io::ErrorKind::PermissionDenied, FileErrorKind::Inaccessible),
      (io::ErrorKind::ResourceBusy, FileErrorKind::Inaccessible),
      (io::ErrorKind::StorageFull, FileErrorKind::DiskFull),
      (io::ErrorKind::QuotaExceeded, FileErrorKind::DiskFull),
      (io::ErrorKind::UnexpectedEof, FileErrorKind::Other),
    ];
    for (kind, expected) in cases {
      assert_eq!(classify(&io::Error::from(kind)), expected, "{kind:?}");
    }
  }

  #[test]
  fn paths_inside_the_instance_are_relative_and_outside_ones_absolute() {
    let root = Path::new(if cfg!(windows) {
      r"C:\games\inst"
    } else {
      "/games/inst"
    });
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(
      display_path(&root.join("mods").join("a.jar"), Some(root)),
      format!("mods{sep}a.jar")
    );
    // The staging area is shown from the update folder on, with or without a root.
    let staged = root.join(UPDATE_DIR).join("staging").join("mods/a.jar");
    let expected = format!("{UPDATE_DIR}{sep}staging{sep}mods{sep}a.jar");
    assert_eq!(display_path(&staged, Some(root)), expected);
    assert_eq!(display_path(&staged, None), expected);
    let outside = if cfg!(windows) {
      PathBuf::from(r"C:\Users\x\AppData\Roaming\USTBL\modpack-publish\7.zip")
    } else {
      PathBuf::from("/home/x/modpack-publish/7.zip")
    };
    assert_eq!(
      display_path(&outside, Some(root)),
      outside.to_string_lossy()
    );
  }

  #[test]
  fn a_tagged_error_keeps_its_kind_and_round_trips_through_text() {
    let root = Path::new("inst");
    let source = io::Error::new(io::ErrorKind::PermissionDenied, "denied (os error 5)");
    let tagged = Err::<(), _>(source)
      .at_in(&root.join("mods").join("a.jar"), root)
      .unwrap_err();
    assert_eq!(tagged.kind(), io::ErrorKind::PermissionDenied);
    let text = crate::error::USTBLError::from(tagged).0;
    let sep = std::path::MAIN_SEPARATOR;
    assert!(text.starts_with("FILE_INACCESSIBLE:{"), "{text}");
    assert_eq!(
      parse(&text),
      Some(ParsedFileError {
        kind: FileErrorKind::Inaccessible,
        file: format!("mods{sep}a.jar"),
        detail: "denied (os error 5)".into(),
      })
    );
  }

  #[test]
  fn quotes_and_non_ascii_survive_and_trailing_text_is_ignored() {
    let error = io::Error::new(io::ErrorKind::Other, "bad \"thing\" 失败");
    let text = Err::<(), _>(error)
      .at(Path::new("mods/航空学 \"x\".jar"))
      .unwrap_err()
      .to_string();
    let with_tail = format!("{text}; rollback failed: other");
    let parsed = parse(&with_tail).unwrap();
    assert_eq!(parsed.kind, FileErrorKind::Other);
    assert!(parsed.file.contains("航空学 \"x\".jar"));
    assert_eq!(parsed.detail, "bad \"thing\" 失败");
  }

  #[test]
  fn plain_strings_from_older_records_are_not_file_errors() {
    for text in [
      "",
      "MODPACK_UPDATE_BUSY",
      "FILE_OPERATION_ERROR",
      "No such file (os error 2)",
      "FILE_MISSING:not json",
      "SOMETHING:{\"file\":\"a\",\"detail\":\"b\"}",
    ] {
      assert_eq!(parse(text), None, "{text}");
    }
  }

  #[test]
  fn the_first_path_wins_and_a_later_root_is_filled_in() {
    let root = Path::new("inst");
    let file = root.join("mods").join("a.jar");
    let inner = Err::<(), _>(io::Error::from(io::ErrorKind::NotFound))
      .at(&file)
      .unwrap_err();
    let rooted = Err::<(), _>(inner)
      .at(Path::new("elsewhere"))
      .rooted(root)
      .unwrap_err();
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(
      parse(&rooted.to_string()).unwrap().file,
      format!("mods{sep}a.jar")
    );
  }

  #[test]
  fn logic_errors_are_left_alone() {
    let error = io::Error::new(io::ErrorKind::InvalidData, "bad json");
    let kept = Err::<(), _>(error).at(Path::new("a")).unwrap_err();
    assert_eq!(kept.to_string(), "bad json");
  }

  #[cfg(windows)]
  #[test]
  fn text_ending_in_an_os_error_becomes_a_file_error() {
    let text = from_os_text(
      "file creation failed (os error 225)",
      Path::new("inst/a.jar"),
      Some(Path::new("inst")),
    );
    let parsed = parse(&text).unwrap();
    assert_eq!(parsed.kind, FileErrorKind::Antivirus);
    assert_eq!(parsed.file, "a.jar");
    assert_eq!(parsed.detail, "file creation failed (os error 225)");
    for plain in ["http status 503", "connection reset (os error 10054)"] {
      assert_eq!(from_os_text(plain, Path::new("a"), None), plain);
    }
  }
}
