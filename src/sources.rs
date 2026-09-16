//! Source checkouts: codeload tarball download, extraction and the GitHub archived check.
//!
//! No git is needed. A checkout is a directory into which the repository tarball was
//! extracted with the top-level wrapper directory stripped; the commit it corresponds to comes
//! from the tarball's pax `comment` global header (git archive writes it) and, failing that,
//! from the wrapper directory suffix.
//!
//! All network access goes through the [`Fetcher`] trait so that tests and
//! `resolve --from-manifest` runs can use an in-memory implementation.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use flate2::read::GzDecoder;
use tar::{Archive, EntryType, PaxExtensions};
use thiserror::Error;

use crate::config::RepoSlug;

const USER_AGENT: &str = concat!("pinakes/", env!("CARGO_PKG_VERSION"));

/// Errors raised while fetching or extracting a source.
#[derive(Debug, Error)]
pub enum SourceError {
    /// An HTTP request failed or returned a non-success status.
    #[error("{url}: {message}")]
    Http {
        /// The requested URL.
        url: String,
        /// What went wrong, including a hint for 404s.
        message: String,
    },
    /// The tarball could not be read or extracted.
    #[error("extracting tarball: {0}")]
    Tar(#[source] io::Error),
    /// A filesystem operation under the checkout directory failed.
    #[error("{path}: {source}")]
    Io {
        /// The path being written or read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The tarball contained an entry that would escape the checkout directory.
    #[error("tarball entry {0:?} is not a safe relative path")]
    UnsafePath(String),
    /// Neither a pax `comment` header nor a wrapper directory identified the commit.
    #[error("cannot determine the commit of the tarball for {0}")]
    NoCommit(String),
    /// The fetcher has no tarball for this repository and ref.
    #[error("no tarball for {slug} at {git_ref}")]
    NotFound {
        /// Requested repository.
        slug: String,
        /// Requested ref.
        git_ref: String,
    },
}

/// Where tarballs and repository metadata come from.
pub trait Fetcher {
    /// Open the gzipped tarball of `slug` at `git_ref` (branch, tag or SHA).
    fn tarball(&self, slug: &RepoSlug, git_ref: &str) -> Result<Box<dyn Read>, SourceError>;

    /// Whether the repository is archived upstream; `None` when that cannot be determined.
    fn archived(&self, slug: &RepoSlug) -> Option<bool>;
}

/// The real fetcher: codeload.github.com for tarballs, api.github.com for metadata.
pub struct GitHubFetcher {
    agent: ureq::Agent,
    token: Option<String>,
}

impl Default for GitHubFetcher {
    fn default() -> Self {
        GitHubFetcher::new()
    }
}

impl GitHubFetcher {
    /// A fetcher that uses `GITHUB_TOKEN` from the environment when it is set and non-empty.
    pub fn new() -> GitHubFetcher {
        let token = std::env::var("GITHUB_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty());
        GitHubFetcher::with_token(token)
    }

    /// A fetcher with an explicit token (or none).
    pub fn with_token(token: Option<String>) -> GitHubFetcher {
        let agent = ureq::Agent::config_builder()
            .user_agent(USER_AGENT)
            .build()
            .new_agent();
        GitHubFetcher { agent, token }
    }

    fn get(
        &self,
        url: &str,
        timeout: Option<Duration>,
    ) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let mut request = self.agent.get(url).config().timeout_global(timeout).build();
        if let Some(token) = &self.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        request
    }
}

fn http_error(url: &str, err: &ureq::Error) -> SourceError {
    let message = match err {
        ureq::Error::StatusCode(404) => {
            "HTTP 404 (repository or ref not found, or private without GITHUB_TOKEN)".to_string()
        }
        ureq::Error::StatusCode(code) => format!("HTTP {code}"),
        other => other.to_string(),
    };
    SourceError::Http {
        url: url.to_string(),
        message,
    }
}

impl Fetcher for GitHubFetcher {
    fn tarball(&self, slug: &RepoSlug, git_ref: &str) -> Result<Box<dyn Read>, SourceError> {
        let url = format!("https://codeload.github.com/{slug}/tar.gz/{git_ref}");
        let response = self
            .get(&url, None)
            .call()
            .map_err(|e| http_error(&url, &e))?;
        Ok(Box::new(response.into_body().into_reader()))
    }

    fn archived(&self, slug: &RepoSlug) -> Option<bool> {
        let url = format!("https://api.github.com/repos/{slug}");
        let response = self
            .get(&url, Some(Duration::from_secs(30)))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call()
            .ok()?;
        let body: serde_json::Value = response.into_body().read_json().ok()?;
        body.get("archived")?.as_bool()
    }
}

/// An extracted repository on disk.
#[derive(Debug, Clone)]
pub struct Checkout {
    /// Directory holding the repository contents (wrapper stripped).
    pub root: PathBuf,
    /// The commit the tarball was made from.
    pub commit: String,
}

/// Download and extract `slug` at `git_ref` into `dest`.
pub fn fetch_checkout(
    fetcher: &dyn Fetcher,
    slug: &RepoSlug,
    git_ref: &str,
    dest: &Path,
) -> Result<Checkout, SourceError> {
    let reader = fetcher.tarball(slug, git_ref)?;
    let commit = extract_tarball(reader, &slug.repo, dest)?;
    Ok(Checkout {
        root: dest.to_path_buf(),
        commit,
    })
}

/// Extract a gzipped tarball into `dest`, stripping the top-level wrapper directory, and return
/// the commit it was made from.
///
/// `repo` is the repository name, used to strip the `<repo>-` prefix from the wrapper when no
/// pax `comment` header is present.
pub fn extract_tarball(reader: impl Read, repo: &str, dest: &Path) -> Result<String, SourceError> {
    let mut archive = Archive::new(GzDecoder::new(reader));
    let mut pax_comment: Option<String> = None;
    let mut wrapper: Option<String> = None;
    std::fs::create_dir_all(dest).map_err(|source| SourceError::Io {
        path: dest.into(),
        source,
    })?;

    for entry in archive.entries().map_err(SourceError::Tar)? {
        let mut entry = entry.map_err(SourceError::Tar)?;
        let kind = entry.header().entry_type();
        if kind == EntryType::XGlobalHeader {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(SourceError::Tar)?;
            if let Some(comment) = pax_value(&bytes, "comment") {
                pax_comment = Some(comment);
            }
            continue;
        }
        let raw = entry.path().map_err(SourceError::Tar)?.into_owned();
        let Some(relative) = strip_wrapper(&raw, &mut wrapper)? else {
            continue;
        };
        let target = dest.join(&relative);
        match kind {
            EntryType::Directory => {
                std::fs::create_dir_all(&target).map_err(|source| SourceError::Io {
                    path: target.clone(),
                    source,
                })?;
            }
            EntryType::Regular | EntryType::Continuous | EntryType::GNUSparse => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|source| SourceError::Io {
                        path: parent.into(),
                        source,
                    })?;
                }
                let mut file =
                    std::fs::File::create(&target).map_err(|source| SourceError::Io {
                        path: target.clone(),
                        source,
                    })?;
                io::copy(&mut entry, &mut file).map_err(|source| SourceError::Io {
                    path: target.clone(),
                    source,
                })?;
            }
            // Symlinks, hard links, devices and friends never belong in a documentation corpus.
            _ => {}
        }
    }

    if let Some(commit) = pax_comment {
        return Ok(commit);
    }
    wrapper
        .as_deref()
        .and_then(|w| wrapper_suffix(w, repo))
        .ok_or_else(|| SourceError::NoCommit(repo.to_string()))
}

/// Read one key from a pax extended header block.
fn pax_value(bytes: &[u8], key: &str) -> Option<String> {
    PaxExtensions::new(bytes)
        .filter_map(Result::ok)
        .find(|ext| ext.key().ok() == Some(key))
        .and_then(|ext| ext.value().ok().map(str::to_string))
}

/// The commit suffix of a codeload wrapper directory, `<repo>-<suffix>`.
fn wrapper_suffix(wrapper: &str, repo: &str) -> Option<String> {
    let suffix = wrapper
        .strip_prefix(repo)
        .and_then(|rest| rest.strip_prefix('-'))
        .or_else(|| wrapper.split_once('-').map(|(_, s)| s))?;
    (!suffix.is_empty()).then(|| suffix.to_string())
}

/// Strip the first path component, recording it as the wrapper, and reject unsafe paths.
fn strip_wrapper(raw: &Path, wrapper: &mut Option<String>) -> Result<Option<PathBuf>, SourceError> {
    let mut components = raw.components();
    let first = match components.next() {
        Some(Component::Normal(name)) => name.to_string_lossy().into_owned(),
        Some(_) => return Err(SourceError::UnsafePath(raw.display().to_string())),
        None => return Ok(None),
    };
    if let Some(known) = wrapper.as_deref() {
        if known != first {
            return Err(SourceError::UnsafePath(raw.display().to_string()));
        }
    } else {
        *wrapper = Some(first);
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(name) => relative.push(name),
            Component::CurDir => {}
            _ => return Err(SourceError::UnsafePath(raw.display().to_string())),
        }
    }
    if relative.as_os_str().is_empty() {
        Ok(None)
    } else {
        Ok(Some(relative))
    }
}

/// List every regular file under `root` as a sorted, `/`-separated relative path.
pub fn list_files(root: &Path) -> Result<Vec<String>, SourceError> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|source| SourceError::Io {
            path: dir.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| SourceError::Io {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| SourceError::Io {
                path: path.clone(),
                source,
            })?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                let relative = path.strip_prefix(root).unwrap_or(&path);
                files.push(to_slash_path(relative));
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Render a relative path with `/` separators regardless of platform.
pub fn to_slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Test doubles: an in-memory tarball builder and a fetcher that serves them.
///
/// Public so integration tests and part 2 can build corpora without network access.
pub mod testing {
    use super::{Fetcher, Read, RepoSlug, SourceError};
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use std::sync::Mutex;

    /// Build a gzipped tarball with files under `wrapper/`, optionally with a pax global header
    /// carrying `comment=<commit>` exactly as `git archive` writes it.
    pub fn build_tarball(wrapper: &str, commit: Option<&str>, files: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        if let Some(commit) = commit {
            let record = format!("comment={commit}\n");
            // A pax record is "<total length> <key>=<value>\n" where the length counts itself.
            let base = record.len() + 1;
            let mut len = base + base.to_string().len();
            if len.to_string().len() + base != len {
                len += 1;
            }
            let data = format!("{len} {record}");
            let mut header = tar::Header::new_ustar();
            header.set_entry_type(tar::EntryType::XGlobalHeader);
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "pax_global_header", data.as_bytes())
                .unwrap();
        }
        let mut dir = tar::Header::new_ustar();
        dir.set_entry_type(tar::EntryType::Directory);
        dir.set_size(0);
        dir.set_mode(0o755);
        dir.set_cksum();
        builder
            .append_data(&mut dir, format!("{wrapper}/"), &[][..])
            .unwrap();
        for (path, bytes) in files {
            let mut header = tar::Header::new_ustar();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("{wrapper}/{path}"), *bytes)
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// A fetcher serving tarballs from memory and recording what was requested.
    #[derive(Default)]
    pub struct FakeFetcher {
        tarballs: BTreeMap<(String, String), Vec<u8>>,
        archived: BTreeMap<String, bool>,
        /// Every `(slug, ref)` requested so far, in order.
        pub requests: Mutex<Vec<(String, String)>>,
    }

    impl FakeFetcher {
        /// Register a tarball for `slug` at `git_ref`.
        pub fn add_tarball(&mut self, slug: &str, git_ref: &str, tarball: Vec<u8>) {
            self.tarballs
                .insert((slug.to_string(), git_ref.to_string()), tarball);
        }

        /// Register the archived flag for `slug`; unregistered slugs report unknown.
        pub fn set_archived(&mut self, slug: &str, archived: bool) {
            self.archived.insert(slug.to_string(), archived);
        }
    }

    impl Fetcher for FakeFetcher {
        fn tarball(&self, slug: &RepoSlug, git_ref: &str) -> Result<Box<dyn Read>, SourceError> {
            let key = (slug.to_string(), git_ref.to_string());
            if let Ok(mut requests) = self.requests.lock() {
                requests.push(key.clone());
            }
            match self.tarballs.get(&key) {
                Some(bytes) => Ok(Box::new(Cursor::new(bytes.clone()))),
                None => Err(SourceError::NotFound {
                    slug: key.0,
                    git_ref: key.1,
                }),
            }
        }

        fn archived(&self, slug: &RepoSlug) -> Option<bool> {
            self.archived.get(&slug.to_string()).copied()
        }
    }
}

/// Map of relative path to file bytes, handy for building small checkouts in tests.
pub type FileMap = BTreeMap<String, Vec<u8>>;

#[cfg(test)]
mod tests {
    use super::testing::{FakeFetcher, build_tarball};
    use super::*;

    const SHA: &str = "4427d7ba863973c2cea9da74ed8675c5c74aee77";

    #[test]
    fn extracts_with_wrapper_stripped_and_commit_from_pax_comment() {
        let tarball = build_tarball(
            "handbook-main",
            Some(SHA),
            &[
                ("docs/user/README.md", b"# Handbook\n"),
                ("README.md", b"top\n"),
            ],
        );
        let dir = tempfile::tempdir().unwrap();
        let commit = extract_tarball(&tarball[..], "handbook", dir.path()).unwrap();
        assert_eq!(commit, SHA);
        assert_eq!(
            std::fs::read(dir.path().join("docs/user/README.md")).unwrap(),
            b"# Handbook\n"
        );
        assert!(!dir.path().join("handbook-main").exists());
        assert_eq!(
            list_files(dir.path()).unwrap(),
            ["README.md", "docs/user/README.md"]
        );
    }

    #[test]
    fn falls_back_to_the_wrapper_suffix() {
        let tarball = build_tarball(&format!("handbook-{SHA}"), None, &[("a.md", b"a")]);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            extract_tarball(&tarball[..], "handbook", dir.path()).unwrap(),
            SHA
        );

        // A repository whose name contains dashes still strips only its own prefix.
        let tarball = build_tarball("my-repo-1.2.3", None, &[("a.md", b"a")]);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            extract_tarball(&tarball[..], "my-repo", dir.path()).unwrap(),
            "1.2.3"
        );
    }

    /// A gzipped tar with a single regular entry whose header names `path` verbatim, bypassing
    /// the builder's own `..` check so the extractor's guard is what gets exercised.
    fn raw_tarball(path: &str) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_ustar();
        header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
        header.set_size(1);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &b"x"[..]).unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn rejects_entries_that_escape_the_checkout() {
        for path in [
            "r-main/../evil.md",
            "/etc/passwd",
            "r-main/sub/../../evil.md",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let err = extract_tarball(&raw_tarball(path)[..], "r", dir.path()).unwrap_err();
            assert!(matches!(err, SourceError::UnsafePath(_)), "{path}: {err}");
            assert!(!dir.path().join("../evil.md").exists());
        }
    }

    #[test]
    fn empty_tarball_has_no_commit() {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let tarball = tar::Builder::new(encoder)
            .into_inner()
            .unwrap()
            .finish()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let err = extract_tarball(&tarball[..], "r", dir.path()).unwrap_err();
        assert!(matches!(err, SourceError::NoCommit(_)), "{err}");
        // A wrapper directory alone still yields its suffix.
        let only_wrapper = build_tarball("r-main", None, &[]);
        assert_eq!(
            extract_tarball(&only_wrapper[..], "r", dir.path()).unwrap(),
            "main"
        );
    }

    #[test]
    fn fake_fetcher_serves_checkouts_and_archived_flags() {
        let mut fetcher = FakeFetcher::default();
        let slug = RepoSlug::parse("https://github.com/o/r.git").unwrap();
        fetcher.add_tarball(
            "o/r",
            "main",
            build_tarball("r-main", Some(SHA), &[("x.md", b"x")]),
        );
        fetcher.set_archived("o/r", true);
        let dir = tempfile::tempdir().unwrap();
        let checkout = fetch_checkout(&fetcher, &slug, "main", dir.path()).unwrap();
        assert_eq!(checkout.commit, SHA);
        assert_eq!(checkout.root, dir.path());
        assert!(dir.path().join("x.md").is_file());
        assert_eq!(fetcher.archived(&slug), Some(true));
        assert_eq!(
            fetcher.archived(&RepoSlug::parse("https://github.com/o/z").unwrap()),
            None
        );
        let err = fetch_checkout(&fetcher, &slug, "v9", dir.path()).unwrap_err();
        assert!(matches!(err, SourceError::NotFound { .. }));
        assert_eq!(fetcher.requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn slash_paths_are_platform_independent() {
        assert_eq!(
            to_slash_path(Path::new("a").join("b").join("c.md").as_path()),
            "a/b/c.md"
        );
    }
}
