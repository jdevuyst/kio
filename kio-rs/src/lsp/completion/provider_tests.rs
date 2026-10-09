use super::*;
use crate::lsp::snapshot::{Snapshot, SnapshotIdGen};
use crate::lsp::state::ServerState;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

const PROVIDER: &str = "module provider; pub elab choose : (. & .) -> . { trailing product; trailing product fallback; impl build }";
const CONSUMER: &str = "module app; import provider(choose); fn run() { choose! { () } ";

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    provider_path: PathBuf,
    provider_uri: Uri,
    consumer_uri: Uri,
    state: ServerState,
}

impl Fixture {
    fn new(duplicate_catalog: bool) -> Self {
        Self::with_provider_state(duplicate_catalog, true)
    }

    fn with_provider_state(duplicate_catalog: bool, provider_open: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let provider_path = root.join("provider.kio");
        let consumer_path = root.join("app.kio");
        let provider_uri = crate::lsp::diagnostics::path_to_uri(&provider_path, &root).unwrap();
        let consumer_uri = crate::lsp::diagnostics::path_to_uri(&consumer_path, &root).unwrap();
        std::fs::write(&provider_path, PROVIDER).unwrap();
        let sources = HashMap::from([
            (provider_path.clone(), PROVIDER.to_owned()),
            (consumer_path.clone(), CONSUMER.to_owned()),
        ]);
        let mut file_to_module = BTreeMap::from([
            (provider_path.clone(), "provider".to_owned()),
            (consumer_path.clone(), "app".to_owned()),
        ]);
        if duplicate_catalog {
            file_to_module.insert(root.join("other/provider.kio"), "provider".to_owned());
        }
        let analysis = LspAnalysis {
            position_index: crate::pass::typecheck_full::PositionIndex::new(),
            file_to_module,
            label_reuse_indexes: crate::lsp::label_reuse::indexes_from_sources(&sources),
            sources,
            generated_label_nominals: Default::default(),
            root_package_lowered: crate::pass::resolve::Package::from_parts(BTreeMap::new(), None),
            warnings: Vec::new(),
        };
        let mut state = ServerState::new();
        let mut versions = BTreeMap::from([(consumer_uri.clone(), 1)]);
        let mut source_paths = BTreeMap::from([(consumer_uri.clone(), consumer_path)]);
        if provider_open {
            state.open(provider_uri.clone(), PROVIDER.into(), 1);
            versions.insert(provider_uri.clone(), 1);
            source_paths.insert(provider_uri.clone(), provider_path.clone());
        }
        state.open(consumer_uri.clone(), CONSUMER.into(), 1);
        state.store_analysis(
            &root,
            Snapshot::new(SnapshotIdGen::new().next(), versions).with_source_paths(source_paths),
            analysis,
        );
        Self {
            _directory: directory,
            root,
            provider_path,
            provider_uri,
            consumer_uri,
            state,
        }
    }

    fn labels(&self) -> Vec<String> {
        handle_completion_request(
            &self.consumer_uri,
            &Position::new(0, CONSUMER.len() as u32),
            Some(&self.root),
            &self.state,
            None,
        )
        .unwrap()
        .list
        .items
        .into_iter()
        .map(|item| item.label)
        .collect()
    }

    fn signature(&self) -> Option<String> {
        handle_block_signature_request(
            &self.consumer_uri,
            &Position::new(0, CONSUMER.len() as u32),
            Some(&self.root),
            &self.state,
        )
        .map(|help| help.signatures[0].label.clone())
    }

    fn replace_provider(&mut self, source: &str) {
        self.state.open(self.provider_uri.clone(), source.into(), 2);
    }
}

#[test]
fn current_provider_header_does_not_require_current_typed_facts() {
    let mut fixture = Fixture::new(false);
    assert_eq!(fixture.labels(), ["fallback"]);
    fixture.replace_provider(&PROVIDER.replace("fallback", "alternate"));
    assert_eq!(fixture.labels(), ["alternate"]);
    assert!(
        fixture
            .signature()
            .unwrap()
            .contains("alternate: product block")
    );
    let sources = CompletionSources {
        local: None,
        catalog: fixture.state.analysis_for_file(&fixture.provider_path),
        state: Some(&fixture.state),
    };
    assert!(sources.provider("provider").is_none());
    assert_eq!(
        std::fs::read_to_string(&fixture.provider_path).unwrap(),
        PROVIDER
    );
}

#[test]
fn invalid_current_provider_headers_never_fall_back_to_old_labels() {
    for source in [
        "module renamed;",
        "module nested/provider;",
        "module provider;",
        "module",
        &PROVIDER.replace("pub elab", "elab"),
        &PROVIDER.replace("elab choose", "elab renamed"),
        &format!(
            "{PROVIDER} {}",
            PROVIDER.strip_prefix("module provider;").unwrap()
        ),
        &PROVIDER.replace("trailing product;", "trailing product fallback;"),
    ] {
        let mut fixture = Fixture::new(false);
        fixture.replace_provider(source);
        assert!(fixture.labels().is_empty(), "{source}");
        assert!(fixture.signature().is_none(), "{source}");
    }
    let mut fixture = Fixture::new(true);
    fixture.replace_provider(&PROVIDER.replace("fallback", "alternate"));
    assert!(fixture.labels().is_empty());
    assert!(fixture.signature().is_none());
}

#[test]
fn provider_header_uses_one_canonical_overlay_and_rejects_alias_ambiguity() {
    let mut fixture = Fixture::new(false);
    let alias: Uri = fixture
        .provider_uri
        .as_str()
        .replace("provider.kio", "%70rovider.kio")
        .parse()
        .unwrap();
    assert_ne!(alias, fixture.provider_uri);
    assert_eq!(
        crate::lsp::util::uri_to_canonical(&alias),
        Some(fixture.provider_path.clone())
    );
    fixture.state.close(&fixture.provider_uri);
    fixture
        .state
        .open(alias.clone(), PROVIDER.replace("fallback", "alternate"), 1);
    assert_eq!(fixture.labels(), ["alternate"]);
    fixture
        .state
        .open(fixture.provider_uri.clone(), PROVIDER.into(), 2);
    assert!(fixture.labels().is_empty());
    assert!(fixture.signature().is_none());
    fixture.state.close(&fixture.provider_uri);
    fixture.state.open(alias, "module renamed;".into(), 2);
    assert!(fixture.labels().is_empty());
}

#[test]
fn provider_header_close_reopen_keeps_disk_freshness_boundary() {
    let mut fixture = Fixture::new(false);
    fixture.replace_provider(&PROVIDER.replace("fallback", "alternate"));
    assert_eq!(fixture.labels(), ["alternate"]);
    fixture.state.close(&fixture.provider_uri);
    assert!(fixture.labels().is_empty());
    fixture.replace_provider(&PROVIDER.replace("fallback", "reopened"));
    assert_eq!(fixture.labels(), ["reopened"]);
    fixture.state.close(&fixture.provider_uri);
    assert!(fixture.labels().is_empty());

    let mut fixture = Fixture::with_provider_state(false, false);
    assert_eq!(fixture.labels(), ["fallback"]);
    std::fs::write(
        &fixture.provider_path,
        PROVIDER.replace("fallback", "disk_label"),
    )
    .unwrap();
    assert!(fixture.labels().is_empty());
    std::fs::remove_file(&fixture.provider_path).unwrap();
    assert!(fixture.labels().is_empty());
    fixture.replace_provider(&PROVIDER.replace("fallback", "unsaved"));
    assert_eq!(fixture.labels(), ["unsaved"]);
}

#[cfg(unix)]
#[test]
fn provider_header_validates_the_original_overlay_file_context() {
    let mut fixture = Fixture::new(false);
    let alias_path = fixture.root.join("alias.kio");
    std::os::unix::fs::symlink(&fixture.provider_path, &alias_path).unwrap();
    let alias = crate::lsp::diagnostics::path_to_uri(&alias_path, &fixture.root).unwrap();
    assert_eq!(
        crate::lsp::util::uri_to_canonical(&alias),
        Some(fixture.provider_path.clone())
    );
    fixture.state.close(&fixture.provider_uri);
    fixture
        .state
        .open(alias, PROVIDER.replace("fallback", "alternate"), 1);
    assert!(fixture.labels().is_empty());
    assert!(fixture.signature().is_none());
}
