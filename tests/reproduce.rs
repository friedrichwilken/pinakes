//! `resolve --from-manifest` must reproduce the artifact byte for byte (SPEC §7.4), including
//! sources with a render step (SPEC §10.1, §2.2): the manifest records the render configuration
//! per source, `command`/`args` absolutised as they were actually run, and `--from-manifest`
//! re-runs it before materialising, rather than treating a rendered page like a plain copy.

use std::fs;

use pinakes::artifact::snapshot;
use pinakes::commands::{Paths, ResolveOptions, resolve};
use pinakes::config::Render;
use pinakes::sources::testing::{FakeFetcher, build_tarball};

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SHA_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

const SUBSCRIPTION_CRD: &str = include_str!("fixtures/crds/subscriptions.yaml");

fn fetcher() -> FakeFetcher {
    let glob_files: [(&str, &[u8]); 3] = [
        ("docs/user/README.md", b"# Glob Source\n\nHello.\n"),
        ("docs/user/_sidebar.md", b"- [x](README.md)\n"),
        (
            "docs/user/deep/page.md",
            b"---\ntitle: Deep\n---\n\nBody.\n",
        ),
    ];
    let ext_files: [(&str, &[u8]); 3] = [
        ("docs/a.md", b"# A\n\nstorage\n"),
        ("docs/b.md", b"# B\n\nleft out, mentions storage\n"),
        ("docs/c.md", b"# C\n\nquiet\n"),
    ];
    let mut fetcher = FakeFetcher::default();
    for (git_ref, wrapper) in [("main", "glob-repo-main"), (SHA_A, "glob-repo-sha")] {
        fetcher.add_tarball(
            "acme/glob-repo",
            git_ref,
            build_tarball(wrapper, Some(SHA_A), &glob_files),
        );
    }
    for (git_ref, wrapper) in [("v1.0.0", "ext-repo-1.0.0"), (SHA_B, "ext-repo-sha")] {
        fetcher.add_tarball(
            "acme/ext-repo",
            git_ref,
            build_tarball(wrapper, Some(SHA_B), &ext_files),
        );
    }
    fetcher.set_archived("acme/glob-repo", false);
    fetcher
}

#[test]
fn from_manifest_reproduces_the_artifact_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let resolvers = dir.path().join("resolvers");
    fs::create_dir_all(&resolvers).unwrap();
    fs::write(
        resolvers.join("toc.sh"),
        "#!/bin/sh\n\
         printf '{\"path\":\"docs/a.md\",\"title\":\"Nav A\",\"doc_type\":\"concept\",\"section\":\"Top\"}\\n'\n\
         printf '{\"path\":\"docs/b.md\",\"selected\":false,\"section\":\"Top\"}\\n'\n\
         printf '{\"path\":\"docs/ghost.md\",\"selected\":true}\\n'\n",
    )
    .unwrap();
    let config = "version: 1\n\
        sources:\n\
        \x20 - name: glob-src\n\
        \x20   repo: https://github.com/acme/glob-repo.git\n\
        \x20   ref: main\n\
        \x20   priority: 10\n\
        \x20   resolver:\n\
        \x20     type: glob\n\
        \x20     include: ['docs/user/**/*.md']\n\
        \x20     exclude: ['**/_sidebar.md']\n\
        \x20 - name: ext-src\n\
        \x20   repo: https://github.com/acme/ext-repo.git\n\
        \x20   ref: v1.0.0\n\
        \x20   resolver:\n\
        \x20     type: external\n\
        \x20     command: ['sh', 'resolvers/toc.sh']\n\
        \x20     residue_scope: ['docs/**/*.md']\n\
        \x20     residue_mention: '(?i)storage'\n\
        policy:\n\
        \x20 deny: ['**/CHANGELOG.md']\n";
    let config_path = dir.path().join("pinakes.yaml");
    fs::write(&config_path, config).unwrap();
    let paths = Paths::for_config(&config_path);
    let fetcher = fetcher();

    let options = ResolveOptions {
        from_manifest: None,
        generated_at: Some("2026-09-16T12:00:00Z".into()),
    };
    let outcome = resolve(&paths, &options, &fetcher).unwrap();
    let glob = &outcome.manifest.sources["glob-src"];
    let ext = &outcome.manifest.sources["ext-src"];
    assert_eq!(glob.commit, SHA_A);
    assert_eq!(ext.commit, SHA_B);
    assert_eq!(ext.archived, None, "unknown archived state stays null");
    assert_eq!(glob.pages.len(), 2);
    assert_eq!(ext.pages.len(), 1);
    assert_eq!(ext.pages["docs/a.md"].title, "Nav A");
    assert_eq!(ext.residue, ["docs/b.md"], "c.md lacks the mention");
    assert_eq!(ext.unresolved, ["docs/ghost.md"]);
    let first = snapshot(&paths.artifact).unwrap();
    let expected_files: Vec<&str> = vec![
        "_residue/ext-src/docs/b.md",
        "ext-src/docs/a.md",
        "ext-src/meta.json",
        "glob-src/docs/user/README.md",
        "glob-src/docs/user/deep/page.md",
        "glob-src/meta.json",
        "manifest.json",
    ];
    assert_eq!(
        first.keys().map(String::as_str).collect::<Vec<_>>(),
        expected_files
    );
    let first_manifest = fs::read_to_string(&paths.manifest).unwrap();
    let first_residue = fs::read_to_string(&paths.residue).unwrap();

    // Reproduce into a different directory from the committed manifest, with a fetcher that
    // only knows the recorded commits, and without the resolver script being available.
    let recorded = dir.path().join("recorded.json");
    fs::copy(&paths.manifest, &recorded).unwrap();
    fs::remove_file(resolvers.join("toc.sh")).unwrap();
    let mut second_paths = paths.clone();
    second_paths.artifact = dir.path().join("artifact-2");
    second_paths.manifest = dir.path().join("manifest-2.json");
    second_paths.residue = dir.path().join("residue-2.jsonl");
    let options = ResolveOptions {
        from_manifest: Some(recorded),
        generated_at: None,
    };
    let outcome = resolve(&second_paths, &options, &fetcher).unwrap();
    assert_eq!(outcome.manifest.generated_at, "2026-09-16T12:00:00Z");

    let second = snapshot(&second_paths.artifact).unwrap();
    assert_eq!(first, second, "artifact must be identical byte for byte");
    assert_eq!(
        fs::read_to_string(&second_paths.manifest).unwrap(),
        first_manifest
    );
    assert_eq!(
        fs::read_to_string(&second_paths.residue)
            .unwrap()
            .lines()
            .count(),
        first_residue.lines().count()
    );

    let requests = fetcher.requests.lock().unwrap();
    assert!(requests.contains(&("acme/glob-repo".to_string(), SHA_A.to_string())));
    assert!(requests.contains(&("acme/ext-repo".to_string(), SHA_B.to_string())));
}

/// A source rendered through the built-in `openapi` renderer (SPEC §10.2) must be reproduced
/// byte for byte: `--from-manifest` has no config and no resolver to re-select `subscriptions
/// .yaml`, so it must re-run the render step from the source's recorded `render: {type: openapi}`
/// alone.
#[test]
fn openapi_render_reproduces_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let config = "version: 1\n\
        sources:\n\
        \x20 - name: crds\n\
        \x20   repo: https://github.com/acme/crds-repo.git\n\
        \x20   ref: main\n\
        \x20   resolver:\n\
        \x20     type: glob\n\
        \x20     include: ['config/crd/bases/*.yaml']\n\
        \x20   render:\n\
        \x20     type: openapi\n";
    let config_path = dir.path().join("pinakes.yaml");
    fs::write(&config_path, config).unwrap();
    let paths = Paths::for_config(&config_path);

    let files: [(&str, &[u8]); 1] = [(
        "config/crd/bases/subscriptions.yaml",
        SUBSCRIPTION_CRD.as_bytes(),
    )];
    let mut fetcher = FakeFetcher::default();
    for (git_ref, wrapper) in [("main", "crds-repo-main"), (SHA_C, "crds-repo-sha")] {
        fetcher.add_tarball(
            "acme/crds-repo",
            git_ref,
            build_tarball(wrapper, Some(SHA_C), &files),
        );
    }
    fetcher.set_archived("acme/crds-repo", false);

    let options = ResolveOptions {
        from_manifest: None,
        generated_at: Some("2026-09-16T12:00:00Z".into()),
    };
    let outcome = resolve(&paths, &options, &fetcher).unwrap();
    let crds = &outcome.manifest.sources["crds"];
    assert_eq!(crds.pages.len(), 2, "v1 and v1alpha1 are served");
    assert_eq!(crds.render, Some(Render::Openapi));
    for entry in crds.pages.values() {
        assert_eq!(
            entry.rendered_from.as_deref(),
            Some("config/crd/bases/subscriptions.yaml")
        );
    }

    let first = snapshot(&paths.artifact).unwrap();
    let first_manifest = fs::read_to_string(&paths.manifest).unwrap();

    let recorded = dir.path().join("recorded.json");
    fs::copy(&paths.manifest, &recorded).unwrap();
    let mut second_paths = paths.clone();
    second_paths.artifact = dir.path().join("artifact-2");
    second_paths.manifest = dir.path().join("manifest-2.json");
    second_paths.residue = dir.path().join("residue-2.jsonl");
    let options = ResolveOptions {
        from_manifest: Some(recorded),
        generated_at: None,
    };
    resolve(&second_paths, &options, &fetcher).unwrap();

    let second = snapshot(&second_paths.artifact).unwrap();
    assert_eq!(
        first, second,
        "the rendered artifact must be identical byte for byte"
    );
    assert_eq!(
        fs::read_to_string(&second_paths.manifest).unwrap(),
        first_manifest
    );
}

/// A source rendered through an external command (SPEC §10.1) must also be reproduced byte for
/// byte: the manifest records the command's absolutised path, so `--from-manifest` finds and
/// re-runs it even from a config in a different directory that has nothing relative to it.
#[test]
fn external_render_reproduces_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = dir.path().join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    let script_path = scripts.join("render.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\n\
         mkdir -p \"$PINAKES_OUT/reference\"\n\
         while IFS= read -r line; do\n\
         \x20 path=$(printf '%s' \"$line\" | sed -n 's/.*\"path\": *\"\\([^\"]*\\)\".*/\\1/p')\n\
         \x20 printf '# Rendered\\n\\nfrom %s\\n' \"$path\" > \"$PINAKES_OUT/reference/out.md\"\n\
         \x20 printf '{\"path\":\"reference/out.md\",\"source_path\":\"%s\",\"title\":\"Rendered\",\"doc_type\":\"reference\",\"section\":\"\"}\\n' \"$path\"\n\
         done\n",
    )
    .unwrap();

    let config = "version: 1\n\
        sources:\n\
        \x20 - name: spec\n\
        \x20   repo: https://github.com/acme/spec-repo.git\n\
        \x20   ref: main\n\
        \x20   resolver:\n\
        \x20     type: glob\n\
        \x20     include: ['api/schema.yaml']\n\
        \x20   render:\n\
        \x20     type: external\n\
        \x20     command: ['sh', 'scripts/render.sh']\n";
    let config_path = dir.path().join("pinakes.yaml");
    fs::write(&config_path, config).unwrap();
    let paths = Paths::for_config(&config_path);

    let files: [(&str, &[u8]); 1] = [("api/schema.yaml", b"kind: Whatever\n")];
    let mut fetcher = FakeFetcher::default();
    for (git_ref, wrapper) in [("main", "spec-repo-main"), (SHA_C, "spec-repo-sha")] {
        fetcher.add_tarball(
            "acme/spec-repo",
            git_ref,
            build_tarball(wrapper, Some(SHA_C), &files),
        );
    }
    fetcher.set_archived("acme/spec-repo", false);

    let options = ResolveOptions {
        from_manifest: None,
        generated_at: Some("2026-09-16T12:00:00Z".into()),
    };
    let outcome = resolve(&paths, &options, &fetcher).unwrap();
    let spec = &outcome.manifest.sources["spec"];
    assert_eq!(spec.pages.len(), 1);
    let absolute_script = std::path::absolute(&script_path)
        .unwrap_or(script_path)
        .to_string_lossy()
        .into_owned();
    match &spec.render {
        Some(Render::External { command, args }) => {
            assert_eq!(command, &vec!["sh".to_string(), absolute_script]);
            assert!(args.is_empty());
        }
        other => panic!("expected a recorded external render, got {other:?}"),
    }

    let first = snapshot(&paths.artifact).unwrap();
    let first_manifest = fs::read_to_string(&paths.manifest).unwrap();

    // Reproduce with a config in a different, unrelated directory: `scripts/render.sh` does not
    // exist relative to it, so this only works because the manifest already carries the
    // script's absolute path, recorded as it was actually run at resolve time.
    let elsewhere = dir.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let recorded = elsewhere.join("recorded.json");
    fs::copy(&paths.manifest, &recorded).unwrap();
    let mut second_paths = Paths::for_config(&elsewhere.join("pinakes.yaml"));
    second_paths.artifact = dir.path().join("artifact-2");
    second_paths.manifest = dir.path().join("manifest-2.json");
    second_paths.residue = dir.path().join("residue-2.jsonl");
    let options = ResolveOptions {
        from_manifest: Some(recorded),
        generated_at: None,
    };
    resolve(&second_paths, &options, &fetcher).unwrap();

    let second = snapshot(&second_paths.artifact).unwrap();
    assert_eq!(
        first, second,
        "the externally rendered artifact must be identical byte for byte"
    );
    assert_eq!(
        fs::read_to_string(&second_paths.manifest).unwrap(),
        first_manifest
    );
}
