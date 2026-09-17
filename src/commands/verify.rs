use crate::artifact::{self, MANIFEST_FILE, Problem};
use crate::config::{ArchivedPolicy, Config};
use crate::error::CommandError;
use crate::manifest::Manifest;
use crate::workspace::Paths;

/// The outcome of `verify`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// Reasons the manifest is stale (exit 3).
    pub stale: Vec<String>,
    /// Policy violations (exit 4).
    pub violations: Vec<String>,
}

impl VerifyReport {
    /// Whether verification passed.
    pub fn ok(&self) -> bool {
        self.stale.is_empty() && self.violations.is_empty()
    }
}

/// Run `verify`: the committed manifest must match the config and the artifact (when present),
/// and every policy must hold.
pub fn verify(paths: &Paths, check_artifact: bool) -> Result<VerifyReport, CommandError> {
    let config = Config::load(&paths.config)?;
    let manifest = Manifest::load(&paths.manifest)?;
    let deny = config.deny_set()?;
    let mut report = VerifyReport::default();

    for source in &config.sources {
        match manifest.sources.get(&source.name) {
            None => report
                .stale
                .push(format!("{}: in config but not in manifest", source.name)),
            Some(recorded) => {
                let slug = source.slug().to_string();
                if recorded.repo != slug || recorded.git_ref != source.git_ref {
                    report.stale.push(format!(
                        "{}: manifest records {}@{} but config says {}@{}",
                        source.name, recorded.repo, recorded.git_ref, slug, source.git_ref
                    ));
                }
                if recorded.resolver != source.resolver.kind() {
                    report.stale.push(format!(
                        "{}: manifest resolver {} differs from config {}",
                        source.name,
                        recorded.resolver,
                        source.resolver.kind()
                    ));
                }
            }
        }
    }
    for name in manifest.sources.keys() {
        if config.source(name).is_none() {
            report
                .stale
                .push(format!("{name}: in manifest but not in config"));
        }
    }
    if check_artifact {
        for problem in artifact::check(&paths.artifact, &manifest) {
            let text = match &problem {
                Problem::ManifestDiffers => {
                    format!(
                        "{}: {problem}",
                        paths.artifact.join(MANIFEST_FILE).display()
                    )
                }
                _ => problem.to_string(),
            };
            report.stale.push(text);
        }
    }

    for (name, source) in &manifest.sources {
        if source.pages.len() < config.policy.min_pages_per_source {
            report.violations.push(format!(
                "{name}: {} pages, policy requires at least {}",
                source.pages.len(),
                config.policy.min_pages_per_source
            ));
        }
        if source.archived == Some(true) && config.policy.archived == ArchivedPolicy::Drop {
            report.violations.push(format!(
                "{name}: repository is archived and policy says drop"
            ));
        }
        for path in source.pages.keys() {
            if deny.is_match(path) {
                report
                    .violations
                    .push(format!("{name}::{path}: matches policy.deny"));
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::commands::decide::decide;
    use crate::commands::residue::residue_list;
    use crate::decisions::{self, Decision, Verdict};
    use crate::manifest::SelectedBy;
    use crate::pipeline::resolve;
    use crate::pipeline::testing::{CONFIG, SHA, fetcher, opts, workspace};
    use crate::residue::{ListFilter, Reason};
    use crate::sources::testing::{FakeFetcher, build_tarball};
    use crate::text::sha256_hex;

    #[test]
    fn resolve_then_verify_then_decide() {
        let (_dir, paths) = workspace(CONFIG);
        let fetcher = fetcher();
        let outcome = resolve(&paths, &opts(), &fetcher).unwrap();
        let handbook = &outcome.manifest.sources["handbook"];
        assert_eq!(handbook.commit, SHA);
        assert_eq!(handbook.archived, Some(false));
        assert_eq!(handbook.pages.len(), 2);
        assert_eq!(
            outcome.residue.iter().map(|r| r.reason).collect::<Vec<_>>(),
            [Reason::Excluded, Reason::Excluded],
            "docs/_sidebar.md matches resolver.exclude and docs/adr/1.md matches policy.deny, \
             both now residue in their own right (SPEC §2.4) instead of vanishing silently"
        );
        assert!(outcome.warnings.is_empty());
        assert!(paths.manifest.is_file() && paths.residue.is_file());
        assert!(paths.artifact.join("handbook/docs/a.md").is_file());
        assert!(!paths.artifact.join("handbook/docs/adr/1.md").exists());

        assert!(verify(&paths, true).unwrap().ok());

        // Policy: raise the minimum so the source violates it.
        fs::write(
            &paths.config,
            CONFIG.replace("min_pages_per_source: 2", "min_pages_per_source: 3"),
        )
        .unwrap();
        let report = verify(&paths, true).unwrap();
        assert!(report.stale.is_empty());
        assert_eq!(report.violations.len(), 1, "{report:?}");

        // Stale: change the ref in the config and tamper with the artifact.
        fs::write(&paths.config, CONFIG.replace("ref: main", "ref: v2")).unwrap();
        fs::write(paths.artifact.join("handbook/docs/a.md"), "tampered").unwrap();
        let report = verify(&paths, true).unwrap();
        assert_eq!(report.stale.len(), 2, "{report:?}");
        assert_eq!(verify(&paths, false).unwrap().stale.len(), 1);

        fs::write(&paths.config, CONFIG).unwrap();
        let decision = decide(
            &paths,
            "handbook::docs/a.md",
            Verdict::Exclude,
            "noise",
            "me",
            Some("t".into()),
        )
        .unwrap();
        assert_eq!(decision.sha256, sha256_hex(b"# A\n"));
        assert!(matches!(
            decide(&paths, "handbook::nope.md", Verdict::Exclude, "", "", None).unwrap_err(),
            CommandError::UnknownId(_)
        ));

        // The exclude decision now removes the page and reports it as residue, alongside the
        // ongoing policy.deny and resolver.exclude residue (SPEC §2.4).
        let outcome = resolve(&paths, &opts(), &fetcher).unwrap();
        assert_eq!(outcome.manifest.sources["handbook"].pages.len(), 1);
        assert_eq!(outcome.residue.len(), 3, "{:#?}", outcome.residue);
        assert_eq!(outcome.residue[0].id, "handbook::docs/a.md");
        assert!(
            residue_list(&paths, &ListFilter::default())
                .unwrap()
                .is_empty(),
            "decided → hidden; the excluded ones are hidden by default regardless of decisions"
        );
        assert!(paths.artifact.join("_residue/handbook/docs/a.md").is_file());
        assert!(outcome.expired.is_empty());

        // A decision with a stale hash is reported as expired and ignored.
        decisions::append(
            &paths.decisions,
            &Decision {
                id: "handbook::docs/b.md".into(),
                sha256: "stale".into(),
                decision: Verdict::Exclude,
                reason: String::new(),
                by: String::new(),
                at: String::new(),
            },
        )
        .unwrap();
        let outcome = resolve(&paths, &opts(), &fetcher).unwrap();
        assert_eq!(outcome.expired.len(), 1);
        assert_eq!(outcome.expired[0].decision.id, "handbook::docs/b.md");
        assert!(
            outcome.manifest.sources["handbook"]
                .pages
                .contains_key("docs/b.md")
        );
    }

    const CRD_CONFIG: &str = "version: 1\nsources:\n  - name: crds\n    repo: https://github.com/o/crds.git\n    \
                               ref: main\n    resolver:\n      type: glob\n      \
                               include: ['config/crd/bases/*.yaml']\n    render:\n      type: openapi\n";

    const CRD_YAML: &[u8] = b"apiVersion: apiextensions.k8s.io/v1\n\
kind: CustomResourceDefinition\n\
metadata:\n  name: widgets.example.com\n\
spec:\n  group: example.com\n  names:\n    kind: Widget\n    plural: widgets\n  scope: Namespaced\n  \
versions:\n    - name: v1\n      served: true\n      storage: true\n      schema:\n        \
openAPIV3Schema:\n          type: object\n          properties:\n            spec:\n              \
type: object\n              properties:\n                size:\n                  type: string\n";

    #[test]
    fn resolve_renders_crds_and_records_unrendered_selected_files() {
        let (_dir, paths) = workspace(CRD_CONFIG);
        let files: [(&str, &[u8]); 2] = [
            ("config/crd/bases/widgets.yaml", CRD_YAML),
            ("config/crd/bases/not-a-crd.yaml", b"foo: bar\n"),
        ];
        let mut fetcher = FakeFetcher::default();
        fetcher.add_tarball(
            "o/crds",
            "main",
            build_tarball("crds-main", Some(SHA), &files),
        );
        fetcher.set_archived("o/crds", false);

        let outcome = resolve(&paths, &opts(), &fetcher).unwrap();
        let crds = &outcome.manifest.sources["crds"];
        assert_eq!(crds.pages.len(), 1, "one page per served CRD version");
        assert_eq!(crds.unrendered, ["config/crd/bases/not-a-crd.yaml"]);

        let page = &crds.pages["reference/example.com/widget-v1.md"];
        assert_eq!(page.title, "Widget (example.com/v1)");
        assert_eq!(page.doc_type, "reference");
        assert_eq!(page.section, "example.com");
        assert_eq!(page.selected_by, SelectedBy::Include);
        assert_eq!(
            page.rendered_from.as_deref(),
            Some("config/crd/bases/widgets.yaml")
        );

        let rendered = fs::read_to_string(
            paths
                .artifact
                .join("crds/reference/example.com/widget-v1.md"),
        )
        .unwrap();
        assert_eq!(sha256_hex(rendered.as_bytes()), page.sha256);
        assert!(rendered.starts_with("# Widget (example.com/v1)\n"));
        assert!(
            !paths
                .artifact
                .join("crds/config/crd/bases/widgets.yaml")
                .exists()
        );

        let meta = fs::read_to_string(paths.artifact.join("crds/meta.json")).unwrap();
        assert!(meta.contains("\"unrendered\": [\n    \"config/crd/bases/not-a-crd.yaml\"\n  ]"));
        assert!(verify(&paths, true).unwrap().ok());
    }
}
