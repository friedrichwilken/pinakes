//! JSON Lines: one JSON object per line, read and written the same way everywhere.
//!
//! Every `.jsonl` file pinakes touches follows the same rules, which used to be re-implemented
//! per module:
//!
//! - **Reading.** Lines are split with [`str::lines`]; a line that is empty after trimming is
//!   skipped but still counted, so reported line numbers are the one-based numbers an editor
//!   shows. The first line that does not deserialise stops the read.
//! - **Writing.** One compact object per item, each followed by `\n` (so a non-empty file ends
//!   in a newline and an empty list gives an empty file). [`KeyOrder`] picks between sorted keys
//!   (the byte-stable form of the generated files) and the struct's declared field order.
//!   Callers sort the *items* themselves; nothing here reorders them.
//! - **Files.** [`write()`] replaces the file and [`append`] creates it when missing; neither
//!   creates parent directories. [`read`] fails on a missing file, [`read_or_empty`] treats it
//!   as holding no items.
//!
//! This module depends on nothing else in the crate. Modules keep their own public error enums
//! and map [`JsonlError`] into them, so user-visible messages stay theirs.

use std::io::Write;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

/// Errors raised while reading or writing a JSONL file.
#[derive(Debug, Error)]
pub enum JsonlError {
    /// The file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The JSONL file path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A line is not valid JSON for the item type, or an item could not be serialised.
    #[error("{path}:{line}: invalid JSONL: {source}")]
    Json {
        /// The JSONL file path.
        path: PathBuf,
        /// One-based line number; `0` when an item could not be serialised for writing.
        line: usize,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
}

/// A line of JSONL text that did not deserialise; [`JsonlError::Json`] without the path.
#[derive(Debug, Error)]
#[error("line {line}: {source}")]
pub struct LineError {
    /// One-based line number.
    pub line: usize,
    /// Underlying JSON error.
    #[source]
    pub source: serde_json::Error,
}

impl LineError {
    /// Attach the file the text came from.
    pub fn at(self, path: &Path) -> JsonlError {
        JsonlError::Json {
            path: path.to_path_buf(),
            line: self.line,
            source: self.source,
        }
    }
}

/// The order of an object's keys when it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOrder {
    /// Keys sorted alphabetically at every level.
    Sorted,
    /// Keys in the order the type serialises them (a struct's declared field order).
    Declared,
}

/// Lazy iterator over the items of JSONL text; see [`parse_lines`].
#[derive(Debug)]
pub struct Lines<'a, T> {
    lines: std::iter::Enumerate<std::str::Lines<'a>>,
    item: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> Iterator for Lines<'_, T> {
    type Item = Result<(usize, T), LineError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (index, line) = self.lines.next()?;
            if line.trim().is_empty() {
                continue;
            }
            let line_no = index + 1;
            return Some(match serde_json::from_str(line) {
                Ok(item) => Ok((line_no, item)),
                Err(source) => Err(LineError {
                    line: line_no,
                    source,
                }),
            });
        }
    }
}

/// Parse `text` lazily, yielding each non-blank line's item with its one-based line number.
///
/// Use this instead of [`parse`] when each item is validated further and the first problem in
/// file order, of either kind, must be the one reported.
pub fn parse_lines<T: DeserializeOwned>(text: &str) -> Lines<'_, T> {
    Lines {
        lines: text.lines().enumerate(),
        item: PhantomData,
    }
}

/// Parse every non-blank line of `text` in order.
pub fn parse<T: DeserializeOwned>(text: &str) -> Result<Vec<T>, LineError> {
    parse_lines(text)
        .map(|item| item.map(|(_, item)| item))
        .collect()
}

/// Read every item of the file at `path`; a missing file is an error.
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, JsonlError> {
    let text = std::fs::read_to_string(path).map_err(io(path))?;
    parse(&text).map_err(|err| err.at(path))
}

/// Read every item of the file at `path`; a missing file yields no items.
pub fn read_or_empty<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, JsonlError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|err| err.at(path)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(io(path)(source)),
    }
}

/// Serialise `items` in the given order, one compact object and a `\n` per item.
pub fn to_string<T: Serialize>(items: &[T], keys: KeyOrder) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    for item in items {
        let line = match keys {
            KeyOrder::Sorted => serde_json::to_string(&serde_json::to_value(item)?)?,
            KeyOrder::Declared => serde_json::to_string(item)?,
        };
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

/// Replace the file at `path` with `items`. The parent directory must exist.
pub fn write<T: Serialize>(path: &Path, items: &[T], keys: KeyOrder) -> Result<(), JsonlError> {
    let text = to_string(items, keys).map_err(serialise(path))?;
    std::fs::write(path, text).map_err(io(path))
}

/// Append `items` to the file at `path`, creating it when needed (also when `items` is empty).
///
/// Everything is serialised before the file is opened, so a serialisation error leaves the file
/// untouched.
pub fn append<T: Serialize>(path: &Path, items: &[T], keys: KeyOrder) -> Result<(), JsonlError> {
    let text = to_string(items, keys).map_err(serialise(path))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io(path))?;
    file.write_all(text.as_bytes()).map_err(io(path))
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> JsonlError + '_ {
    move |source| JsonlError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn serialise(path: &Path) -> impl Fn(serde_json::Error) -> JsonlError + '_ {
    move |source| JsonlError::Json {
        path: path.to_path_buf(),
        line: 0,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Item {
        name: String,
        #[serde(default)]
        count: u32,
    }

    fn item(name: &str, count: u32) -> Item {
        Item {
            name: name.to_string(),
            count,
        }
    }

    #[test]
    fn blank_lines_are_skipped_but_counted() {
        let text = "\n{\"name\":\"a\"}\n   \n\r\n{\"name\":\"b\",\"count\":2}\r\n";
        let numbered: Vec<(usize, Item)> = parse_lines(text).map(Result::unwrap).collect();
        assert_eq!(numbered, vec![(2, item("a", 0)), (5, item("b", 2))]);
        assert_eq!(
            parse::<Item>(text).unwrap(),
            vec![item("a", 0), item("b", 2)]
        );
        assert!(parse::<Item>("").unwrap().is_empty());
    }

    #[test]
    fn a_bad_line_is_reported_with_its_one_based_number() {
        let err = parse::<Item>("{\"name\":\"a\"}\n\nnot json\n").unwrap_err();
        assert_eq!(err.line, 3);
        assert!(err.to_string().starts_with("line 3: "));
    }

    #[test]
    fn parse_lines_is_lazy_so_callers_see_problems_in_file_order() {
        let mut lines = parse_lines::<Item>("{\"name\":\"a\"}\nbroken\n{\"name\":\"c\"}\n");
        assert_eq!(lines.next().unwrap().unwrap(), (1, item("a", 0)));
        assert_eq!(lines.next().unwrap().unwrap_err().line, 2);
        assert_eq!(lines.next().unwrap().unwrap(), (3, item("c", 0)));
        assert!(lines.next().is_none());
    }

    #[test]
    fn read_names_the_path_and_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("items.jsonl");
        std::fs::write(&path, "{\"name\":\"a\"}\n{\"name\":1}\n").unwrap();
        let err = read::<Item>(&path).unwrap_err();
        assert!(matches!(err, JsonlError::Json { line: 2, .. }));
        let prefix = format!("{}:2: invalid JSONL: ", path.display());
        assert!(err.to_string().starts_with(&prefix), "{err}");
    }

    #[test]
    fn a_missing_file_is_an_error_for_read_and_empty_for_read_or_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.jsonl");
        let err = read::<Item>(&path).unwrap_err();
        assert!(matches!(err, JsonlError::Io { .. }));
        assert!(err.to_string().starts_with(&path.display().to_string()));
        assert!(read_or_empty::<Item>(&path).unwrap().is_empty());

        std::fs::write(&path, "{\"name\":\"a\"}\n").unwrap();
        assert_eq!(read_or_empty::<Item>(&path).unwrap(), vec![item("a", 0)]);
        std::fs::write(&path, "nope\n").unwrap();
        assert!(matches!(
            read_or_empty::<Item>(&path).unwrap_err(),
            JsonlError::Json { line: 1, .. }
        ));
    }

    #[test]
    fn key_order_is_sorted_or_declared() {
        let items = [item("b", 1), item("a", 2)];
        assert_eq!(
            to_string(&items, KeyOrder::Sorted).unwrap(),
            "{\"count\":1,\"name\":\"b\"}\n{\"count\":2,\"name\":\"a\"}\n"
        );
        assert_eq!(
            to_string(&items, KeyOrder::Declared).unwrap(),
            "{\"name\":\"b\",\"count\":1}\n{\"name\":\"a\",\"count\":2}\n"
        );
        assert_eq!(to_string::<Item>(&[], KeyOrder::Sorted).unwrap(), "");
    }

    #[test]
    fn to_string_accepts_references_so_callers_can_sort_first() {
        let items = [item("b", 1), item("a", 2)];
        let mut sorted: Vec<&Item> = items.iter().collect();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(
            to_string(&sorted, KeyOrder::Declared).unwrap(),
            "{\"name\":\"a\",\"count\":2}\n{\"name\":\"b\",\"count\":1}\n"
        );
    }

    #[test]
    fn write_replaces_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("items.jsonl");
        write(&path, &[item("a", 1), item("b", 2)], KeyOrder::Sorted).unwrap();
        write(&path, &[item("c", 3)], KeyOrder::Sorted).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"count\":3,\"name\":\"c\"}\n"
        );
        assert_eq!(read::<Item>(&path).unwrap(), vec![item("c", 3)]);
    }

    #[test]
    fn write_and_append_do_not_create_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("items.jsonl");
        let items = [item("a", 1)];
        assert!(matches!(
            write(&path, &items, KeyOrder::Sorted).unwrap_err(),
            JsonlError::Io { .. }
        ));
        assert!(matches!(
            append(&path, &items, KeyOrder::Sorted).unwrap_err(),
            JsonlError::Io { .. }
        ));
    }

    #[test]
    fn append_creates_the_file_and_adds_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("items.jsonl");
        append::<Item>(&path, &[], KeyOrder::Declared).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        append(&path, &[item("a", 1)], KeyOrder::Declared).unwrap();
        append(&path, &[item("b", 2), item("c", 3)], KeyOrder::Declared).unwrap();
        assert_eq!(
            read::<Item>(&path).unwrap(),
            vec![item("a", 1), item("b", 2), item("c", 3)]
        );
        assert!(std::fs::read_to_string(&path).unwrap().ends_with("}\n"));
    }

    #[test]
    fn a_serialisation_error_is_line_zero_and_leaves_the_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("items.jsonl");
        let mut bad = std::collections::BTreeMap::new();
        bad.insert((1, 2), "tuple keys are not JSON");
        let err = append(&path, &[bad], KeyOrder::Declared).unwrap_err();
        assert!(matches!(err, JsonlError::Json { line: 0, .. }));
        assert!(!path.exists());
    }
}
