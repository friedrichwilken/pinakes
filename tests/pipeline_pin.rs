//! Pins the BYTES of what the compile pipeline writes about pages: `residue.jsonl`,
//! `duplicates.jsonl`, `manifest.json`, the `report` Markdown and the `residue list` output,
//! after a fresh `resolve` and again after `resolve --from-manifest`.
//!
//! This is a characterisation test: the expected files under `tests/snapshots/pipeline_pin/`
//! were generated from the code as it was, not written by hand, so that a refactor of how page
//! metadata travels through the pipeline can prove it changed nothing. One workspace exercises
//! as many page shapes as practical in a single resolve:
//!
//! - `a-b` and `a`: two `glob` sources whose id order (`a-b::…` < `a::…`) differs from their
//!   (source, path) order, with selected pages, in-scope pages that are not selected, a
//!   `resolver.exclude` hit, a `policy.deny` hit, an exact duplicate, a near-duplicate and a
//!   same-title mirror across sources, an `include` decision and an `exclude` decision that
//!   apply, and decisions that expired because the page changed, vanished or never had bytes;
//! - `book`: an `mdbook` navigation source with an unlinked page and a dangling link;
//! - `ext`: an `external` resolver that selects an existing file and a missing one, reports a
//!   `rule` of its own on one candidate, leaves one file unmentioned, and has `include` and
//!   `exclude` globs of its own;
//! - `crds`: a source rendered by the built-in `openapi` renderer, with an unrendered file;
//! - `gone`: a repository reported as archived, under `policy.archived: drop`;
//! - `fresh`: a source the previous manifest does not know, so its residue is `new_source`; it
//!   also holds an exact duplicate of a page in `a-b`, which ties on priority and `selected_by`,
//!   so that the winner rule is pinned down to its last criterion, the id (`ext` pins the
//!   second one, `selected_by`, with a page it selects twice).
//!
//! Everything is offline and deterministic: a [`FakeFetcher`] serves in-memory tarballs, the
//! external resolver is a `sh` script written into the temp dir, and `generated_at` is fixed.
//! No absolute path reaches any pinned file, so nothing is normalised.
//!
//! Refresh with `UPDATE_SNAPSHOTS=1 cargo test --test pipeline_pin`, and only when a change of
//! these bytes is intended (see `AGENTS.md`).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use pinakes::artifact::snapshot;
use pinakes::commands::{Paths, ReportOptions, ResolveOptions, report, residue_list, resolve};
use pinakes::residue::{ListFilter, to_jsonl};
use pinakes::sources::testing::{FakeFetcher, build_tarball};
use pinakes::text::sha256_hex;

const SHA_A_OLD: &str = "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
const SHA_A: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const SHA_AB: &str = "abababababababababababababababababababab";
const SHA_BOOK: &str = "b00cb00cb00cb00cb00cb00cb00cb00cb00cb00c";
const SHA_EXT: &str = "e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1";
const SHA_CRDS: &str = "c4d5c4d5c4d5c4d5c4d5c4d5c4d5c4d5c4d5c4d5";
const SHA_GONE: &str = "9090909090909090909090909090909090909090";
const SHA_FRESH: &str = "f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5f4e5";

const GENERATED_AT: &str = "2026-09-16T12:00:00Z";

const SUBSCRIPTION_CRD: &[u8] = include_bytes!("fixtures/crds/subscriptions.yaml");

const NOTIFICATIONS: &[u8] = b"# Email notifications\n\n\
The platform sends email notifications for account events such as password resets, billing \
changes and security alerts. Templates control the wording and layout of every message the \
platform sends. Copy the default template files into your project and edit the subject line, \
the body text and the footer.\n";

/// [`NOTIFICATIONS`] with one more sentence: a near-duplicate, not an exact one.
const NOTIFICATIONS_VARIANT: &[u8] = b"# Sending email\n\n\
The platform sends email notifications for account events such as password resets, billing \
changes and security alerts. Templates control the wording and layout of every message the \
platform sends. Copy the default template files into your project and edit the subject line, \
the body text and the footer. Keep a backup first.\n";

/// The same bytes in `a` and `a-b`: an exact duplicate.
const LICENSE: &[u8] = b"# License terms\n\n\
This documentation is provided under a permissive licence. You may copy, change and share it \
as long as the original notice stays in place and changes are marked as such.\n";

/// In `a-b` and `fresh`, which tie on priority and on `selected_by`: the id breaks the tie.
const ZEBRA: &[u8] = b"# Zebra\n\nThe same page in two sources of equal priority.\n";

/// Twice in `ext`, once selected by the resolver command and once by the `include` glob: the
/// priority ties, so `selected_by` decides.
const FAQ: &[u8] = b"# Questions\n\nHow often are the signing keys rotated? Every ninety days.\n";

const DRAFT: &[u8] = b"# Draft\n\nAn unfinished note about rotating the signing keys.\n";
const KEEP: &[u8] = b"# Keep\n\nA note worth having: revoke the previous key pair right away.\n";
const STALE: &[u8] = b"# Stale\n\nThis note changed after somebody decided about it.\n";

type Files = Vec<(&'static str, &'static [u8])>;

/// The `a` repository as `resolve` sees it now.
fn a_files() -> Files {
    vec![
        ("README.md", b"# Repository A\n\nIn scope, not selected.\n"),
        ("docs/CHANGELOG.md", b"# Changelog\n\n- denied by policy\n"),
        ("docs/_sidebar.md", b"- [Intro](intro.md)\n"),
        (
            "docs/intro.md",
            b"---\ntitle: Introduction\n---\n\nWelcome, again.\nA second line.\n",
        ),
        ("docs/license.md", LICENSE),
        ("docs/notifications.md", NOTIFICATIONS),
        (
            "docs/setup.md",
            b"# Setup guide\n\nInstall the command line client and sign in once.\n",
        ),
        ("notes/draft.md", DRAFT),
        ("notes/keep.md", KEEP),
        ("notes/stale.md", STALE),
    ]
}

/// The `a` repository at the older commit the first resolve sees: `docs/intro.md` differs and
/// `docs/old.md` still exists, so the report's diff has a changed and a removed page.
fn a_files_old() -> Files {
    let mut files: Files = a_files()
        .into_iter()
        .filter(|(path, _)| *path != "docs/intro.md")
        .collect();
    files.push((
        "docs/intro.md",
        b"---\ntitle: Introduction\n---\n\nWelcome.\n",
    ));
    files.push(("docs/old.md", b"# Old page\n\nRemoved upstream later.\n"));
    files
}

fn a_b_files() -> Files {
    vec![
        ("docs/license.md", LICENSE),
        ("docs/notifications.md", NOTIFICATIONS_VARIANT),
        ("docs/zebra.md", ZEBRA),
        ("extra.md", b"# Extra\n\nIn scope, not selected.\n"),
    ]
}

fn book_files() -> Files {
    vec![
        (
            "src/SUMMARY.md",
            b"# Summary\n\n[Introduction](intro.md)\n\n# Guides\n\n\
              - [Setup guide](guide/setup.md)\n  - [Missing chapter](guide/missing.md)\n\
              - [Draft chapter]()\n",
        ),
        ("src/intro.md", b"# Intro heading\n\nWhat the book covers.\n"),
        (
            "src/guide/setup.md",
            b"# Setting things up\n\nUnpack the archive and run the installer as an administrator.\n",
        ),
        (
            "src/orphan.md",
            b"---\ntitle: Orphan\n---\n\nNo chapter links this page.\n",
        ),
    ]
}

fn ext_files() -> Files {
    vec![
        ("docs/a.md", b"# A\n\nAbout storage classes.\n"),
        (
            "docs/b.md",
            b"# B\n\nLeft out by the resolver; mentions storage.\n",
        ),
        (
            "docs/c.md",
            b"# C\n\nAlso left out; mentions storage too.\n",
        ),
        (
            "docs/d.md",
            b"# D\n\nNever mentioned by the resolver; mentions storage.\n",
        ),
        (
            "docs/e.md",
            b"# E\n\nNever mentioned and lacks the residue word.\n",
        ),
        (
            "docs/internal.md",
            b"# Internal\n\nSelected, then excluded; storage.\n",
        ),
        ("docs/questions.md", FAQ),
        ("extra/faq.md", FAQ),
    ]
}

fn crds_files() -> Files {
    vec![
        ("config/crd/bases/not-a-crd.yaml", b"foo: bar\n"),
        ("config/crd/bases/subscriptions.yaml", SUBSCRIPTION_CRD),
    ]
}

fn gone_files() -> Files {
    vec![
        ("docs/CHANGELOG.md", b"# Changelog\n\n- denied\n"),
        (
            "docs/page.md",
            b"# Archived page\n\nWould have been selected.\n",
        ),
        ("other.md", b"# Other\n\nIn scope, not selected.\n"),
    ]
}

fn fresh_files() -> Files {
    vec![
        (
            "README.md",
            b"# Fresh repository\n\nIn scope, not selected.\n",
        ),
        (
            "docs/new.md",
            b"# New page\n\nSelected from a brand new source.\n",
        ),
        ("docs/zebra.md", ZEBRA),
    ]
}

/// The external resolver (SPEC §3), as a POSIX `sh` script run through `sh <path>`, so neither
/// an executable bit nor a shebang lookup is needed.
const RESOLVER_SCRIPT: &str = "#!/bin/sh\n\
printf '%s\\n' '{\"path\":\"docs/a.md\",\"title\":\"Nav A\",\"doc_type\":\"concept\",\"section\":\"Top\"}'\n\
printf '%s\\n' '{\"path\":\"docs/b.md\",\"selected\":false,\"section\":\"Top\",\"rule\":{\"key\":\"toc:outside-match\",\"text\":\"The table of contents lists the page outside the product branch.\"}}'\n\
printf '%s\\n' '{\"path\":\"docs/c.md\",\"selected\":false,\"title\":\"Nav C\",\"context\":\"Top > Left out\"}'\n\
printf '%s\\n' '{\"path\":\"docs/questions.md\",\"section\":\"Top\"}'\n\
printf '%s\\n' '{\"path\":\"docs/internal.md\",\"section\":\"Top\"}'\n\
printf '%s\\n' '{\"path\":\"docs/ghost.md\",\"title\":\"Ghost\",\"section\":\"Top\"}'\n";

/// Source blocks in declaration order, which is deliberately not the sorted order.
const SOURCES: &str = "\
\x20 - name: a-b
\x20   repo: https://github.com/acme/a-b.git
\x20   ref: main
\x20   resolver:
\x20     type: glob
\x20     include: ['docs/**/*.md']
\x20     residue_scope: ['**/*.md']
\x20 - name: a
\x20   repo: https://github.com/acme/a.git
\x20   ref: main
\x20   priority: 10
\x20   resolver:
\x20     type: glob
\x20     include: ['docs/**/*.md']
\x20     exclude: ['**/_sidebar.md']
\x20     residue_scope: ['**/*.md']
\x20 - name: gone
\x20   repo: https://github.com/acme/gone.git
\x20   ref: main
\x20   resolver:
\x20     type: glob
\x20     include: ['docs/**/*.md']
\x20     residue_scope: ['**/*.md']
\x20 - name: ext
\x20   repo: https://github.com/acme/ext.git
\x20   ref: v1.0.0
\x20   resolver:
\x20     type: external
\x20     command: ['sh', 'resolvers/toc.sh']
\x20     residue_scope: ['docs/**/*.md']
\x20     residue_mention: '(?i)storage'
\x20     include: ['extra/*.md']
\x20     exclude: ['docs/internal.md']
\x20 - name: crds
\x20   repo: https://github.com/acme/crds.git
\x20   ref: main
\x20   resolver:
\x20     type: glob
\x20     include: ['config/crd/bases/*.yaml']
\x20   render:
\x20     type: openapi
\x20 - name: book
\x20   repo: https://github.com/acme/book.git
\x20   ref: main
\x20   priority: 5
\x20   resolver:
\x20     type: mdbook
";

const FRESH_SOURCE: &str = "\
\x20 - name: fresh
\x20   repo: https://github.com/acme/fresh.git
\x20   ref: main
\x20   resolver:
\x20     type: glob
\x20     include: ['docs/**/*.md']
\x20     residue_scope: ['**/*.md']
";

const POLICY: &str = "\
policy:
\x20 deny: ['**/CHANGELOG.md']
\x20 archived: drop
";

fn config(with_fresh: bool) -> String {
    let fresh = if with_fresh { FRESH_SOURCE } else { "" };
    format!("version: 1\nsources:\n{SOURCES}{fresh}{POLICY}")
}

fn add(fetcher: &mut FakeFetcher, slug: &str, git_ref: &str, commit: &str, files: &Files) {
    let wrapper = format!("{}-{git_ref}", slug.replace('/', "-"));
    fetcher.add_tarball(slug, git_ref, build_tarball(&wrapper, Some(commit), files));
}

/// A fetcher for the second (pinned) resolve and everything after it: every repository at its
/// ref and at its commit (for `--from-manifest`), plus `a` at the older commit (for the diff in
/// the report). `a_at_main` says which commit of `a` the `main` ref points at.
fn fetcher(a_at_main: &str) -> FakeFetcher {
    let mut fetcher = FakeFetcher::default();
    let (main_files, main_commit) = if a_at_main == SHA_A_OLD {
        (a_files_old(), SHA_A_OLD)
    } else {
        (a_files(), SHA_A)
    };
    add(&mut fetcher, "acme/a", "main", main_commit, &main_files);
    add(&mut fetcher, "acme/a", SHA_A, SHA_A, &a_files());
    add(&mut fetcher, "acme/a", SHA_A_OLD, SHA_A_OLD, &a_files_old());
    for (slug, git_ref, commit, files) in [
        ("acme/a-b", "main", SHA_AB, a_b_files()),
        ("acme/book", "main", SHA_BOOK, book_files()),
        ("acme/ext", "v1.0.0", SHA_EXT, ext_files()),
        ("acme/crds", "main", SHA_CRDS, crds_files()),
        ("acme/gone", "main", SHA_GONE, gone_files()),
        ("acme/fresh", "main", SHA_FRESH, fresh_files()),
    ] {
        add(&mut fetcher, slug, git_ref, commit, &files);
        add(&mut fetcher, slug, commit, commit, &files);
    }
    // `ext` is left unregistered: its archived state is unknown (`null` in the manifest).
    for slug in ["acme/a", "acme/a-b", "acme/book", "acme/crds", "acme/fresh"] {
        fetcher.set_archived(slug, false);
    }
    fetcher.set_archived("acme/gone", true);
    fetcher
}

fn decision_line(id: &str, sha256: &str, verdict: &str, reason: &str) -> String {
    format!(
        "{{\"id\":\"{id}\",\"sha256\":\"{sha256}\",\"decision\":\"{verdict}\",\
         \"reason\":\"{reason}\",\"by\":\"tester\",\"at\":\"2026-09-01T08:00:00Z\"}}\n"
    )
}

/// `decisions.jsonl`: two decisions that apply, and three that have expired in the three
/// possible ways (the page changed, the page is gone, the id is an unresolved link, which has
/// no bytes and therefore the empty hash).
fn decisions() -> String {
    [
        decision_line(
            "a::notes/draft.md",
            &sha256_hex(DRAFT),
            "exclude",
            "unfinished",
        ),
        decision_line("a::notes/keep.md", &sha256_hex(KEEP), "include", "useful"),
        decision_line(
            "a::notes/stale.md",
            &sha256_hex(b"# Stale\n\nWhat the note said back then.\n"),
            "exclude",
            "decided on older bytes",
        ),
        decision_line(
            "a::notes/vanished.md",
            &sha256_hex(b"gone"),
            "exclude",
            "the page no longer exists",
        ),
        decision_line(
            "ext::docs/ghost.md",
            &sha256_hex(b"ghost"),
            "exclude",
            "a dangling link has no bytes",
        ),
    ]
    .concat()
}

/// Compares produced text with the files under `tests/snapshots/pipeline_pin/`, collecting
/// every mismatch so that one run shows all of them.
struct Pins {
    dir: PathBuf,
    update: bool,
    failures: Vec<String>,
}

impl Pins {
    fn new() -> Pins {
        Pins {
            dir: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("snapshots")
                .join("pipeline_pin"),
            update: std::env::var_os("UPDATE_SNAPSHOTS").is_some(),
            failures: Vec::new(),
        }
    }

    fn check(&mut self, name: &str, actual: &str) {
        let path = self.dir.join(name);
        if self.update {
            fs::create_dir_all(&self.dir).unwrap();
            fs::write(&path, actual).unwrap();
            return;
        }
        let expected = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        if expected == actual {
            return;
        }
        let mut message = format!("{name} differs from tests/snapshots/pipeline_pin/{name}:\n");
        let (mut old, mut new) = (expected.lines(), actual.lines());
        for number in 1.. {
            match (old.next(), new.next()) {
                (None, None) => break,
                (a, b) if a == b => {}
                (a, b) => {
                    let _ = writeln!(message, "  line {number}");
                    let _ = writeln!(message, "    expected: {}", a.unwrap_or("<no line>"));
                    let _ = writeln!(message, "    actual:   {}", b.unwrap_or("<no line>"));
                }
            }
        }
        self.failures.push(message);
    }

    fn check_file(&mut self, name: &str, path: &Path) {
        let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        self.check(name, &text);
    }

    fn finish(self) {
        assert!(
            self.failures.is_empty(),
            "{} pinned output(s) changed:\n{}",
            self.failures.len(),
            self.failures.join("\n")
        );
    }
}

fn residue_list_text(paths: &Paths, include_excluded: bool) -> String {
    let filter = ListFilter {
        source: None,
        reason: None,
        include_excluded,
    };
    // Exactly what `pinakes residue list` writes to stdout.
    to_jsonl(&residue_list(paths, &filter).unwrap()).unwrap()
}

/// Pin what the workspace at `paths` says about its pages; `prefix` tells the fresh resolve
/// from the reproduced one.
fn pin_workspace(
    pins: &mut Pins,
    prefix: &str,
    paths: &Paths,
    old_manifest: &Path,
    fetcher: &FakeFetcher,
) {
    pins.check_file(&format!("{prefix}residue.jsonl"), &paths.residue);
    pins.check_file(&format!("{prefix}duplicates.jsonl"), &paths.duplicates);
    pins.check(
        &format!("{prefix}residue_list.jsonl"),
        &residue_list_text(paths, false),
    );
    pins.check(
        &format!("{prefix}residue_list_include_excluded.jsonl"),
        &residue_list_text(paths, true),
    );
    pins.check(
        &format!("{prefix}report.md"),
        &report(paths, &ReportOptions::default(), fetcher).unwrap(),
    );
    // With a previous manifest the report also has the added, removed and changed pages; the
    // changed page's old text is re-fetched at the old commit.
    let with_old = ReportOptions {
        old: Some(old_manifest.to_path_buf()),
        new_artifact: Some(paths.artifact.clone()),
        ..ReportOptions::default()
    };
    pins.check(
        &format!("{prefix}report_with_old.md"),
        &report(paths, &with_old, fetcher).unwrap(),
    );
}

#[test]
fn pipeline_outputs_are_pinned_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let resolvers = root.join("resolvers");
    fs::create_dir_all(&resolvers).unwrap();
    fs::write(resolvers.join("toc.sh"), RESOLVER_SCRIPT).unwrap();
    let config_path = root.join("pinakes.yaml");
    let paths = Paths::for_config(&config_path);
    fs::write(&paths.decisions, decisions()).unwrap();
    let fresh_options = ResolveOptions {
        from_manifest: None,
        generated_at: Some(GENERATED_AT.into()),
    };

    // First resolve: an earlier state of the workspace (no `fresh` source, `a` at its older
    // commit). It only exists to leave a previous manifest behind.
    fs::write(&config_path, config(false)).unwrap();
    resolve(&paths, &fresh_options, &fetcher(SHA_A_OLD)).unwrap();
    let old_manifest = root.join("manifest-old.json");
    fs::copy(&paths.manifest, &old_manifest).unwrap();

    // Second resolve: the pinned one.
    fs::write(&config_path, config(true)).unwrap();
    let fetcher = fetcher(SHA_A);
    let outcome = resolve(&paths, &fresh_options, &fetcher).unwrap();
    assert_eq!(outcome.warnings, ["gone: repository is archived; dropped"]);
    assert!(!outcome.manifest.sources.contains_key("gone"));

    let mut pins = Pins::new();
    pins.check_file("manifest.json", &paths.manifest);
    pin_workspace(&mut pins, "", &paths, &old_manifest, &fetcher);
    let mut expired = String::new();
    for e in &outcome.expired {
        let _ = writeln!(expired, "{} current={:?}", e.decision.id, e.current_sha256);
    }
    pins.check("expired.txt", &expired);

    // Reproduce from the recorded manifest into a second workspace, without the resolver
    // script: `--from-manifest` must not need it.
    let first_artifact = snapshot(&paths.artifact).unwrap();
    let recorded = root.join("recorded.json");
    fs::copy(&paths.manifest, &recorded).unwrap();
    fs::remove_file(resolvers.join("toc.sh")).unwrap();
    let second = root.join("second");
    fs::create_dir_all(&second).unwrap();
    let mut second_paths = Paths::for_config(&second.join("pinakes.yaml"));
    second_paths.config.clone_from(&paths.config);
    second_paths.decisions.clone_from(&paths.decisions);
    let reproduce_options = ResolveOptions {
        from_manifest: Some(recorded),
        generated_at: None,
    };
    resolve(&second_paths, &reproduce_options, &fetcher).unwrap();
    assert_eq!(
        first_artifact,
        snapshot(&second_paths.artifact).unwrap(),
        "the artifact must be reproduced byte for byte"
    );
    assert_eq!(
        fs::read_to_string(&second_paths.manifest).unwrap(),
        fs::read_to_string(&paths.manifest).unwrap(),
        "the manifest must be reproduced byte for byte"
    );
    pin_workspace(
        &mut pins,
        "reproduced_",
        &second_paths,
        &old_manifest,
        &fetcher,
    );

    pins.finish();
}
