//! The model catalogue.
//!
//! The catalogue lives in `models.json` rather than in code. Mirrors go dead —
//! a host moves, a path changes, a region gets blocked — and editing a file is
//! a better answer to that than shipping a new binary. A copy is embedded so a
//! fresh install still works with nothing else on disk.

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;
use verse_core::{Error, ErrorKind, Result};

use crate::Downloader;

/// The catalogue compiled into this binary.
const EMBEDDED: &str = include_str!("../models.json");

/// The `version` this build understands.
///
/// Checked rather than ignored: a file written for a later format could carry
/// fields this build would silently drop, and a quietly half-loaded catalogue
/// is worse than a refusal.
const SUPPORTED_VERSION: u32 = 1;

/// Sizes in `models.json` must come from the host, not from a finished
/// download.
///
/// A transfer size reported by a client can differ from what lands on disk —
/// compression, a resumed request, or a truncated download all produce a
/// plausible-looking number that is not the file's length. Recording one of
/// those makes every later check fail on a file that is perfectly fine. The
/// `Content-Length` the mirror reports is the authority; it is also the only
/// value that can catch a short transfer.
///
/// A place to fetch from.
#[derive(Debug, Clone, Deserialize)]
pub struct Mirror {
    /// Shown in progress and error messages, so failures name the culprit.
    pub name: String,
    /// Host, e.g. `https://hf-mirror.com`.
    pub base_url: String,
    /// Repository path appended to the base URL. Hosts lay the same model out
    /// differently, so this is per-mirror rather than per-model.
    pub repo: String,
}

impl Mirror {
    /// Full URL for a file.
    pub fn url_for(&self, file: &str) -> String {
        format!(
            "{}/{}/{}",
            self.base_url.trim_end_matches('/'),
            self.repo.trim_matches('/'),
            file.trim_start_matches('/')
        )
    }
}

/// One file of a model.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelFile {
    /// Path within the repository.
    pub remote: String,
    /// Name it takes on disk.
    pub local: String,
    /// Expected length in bytes.
    ///
    /// Absent means unknown, which weakens the integrity check to "the file
    /// exists" — worth knowing, since a truncated transfer can still return
    /// HTTP 200.
    #[serde(default)]
    pub size: Option<u64>,
    /// Lowercase hex SHA-256 of the whole file.
    ///
    /// Absent means unverified, which is where this started: a length check
    /// catches a truncated transfer and says nothing at all about a corrupt
    /// one. The digest is computed while the bytes stream past — the same
    /// buffer that is written to disk is fed to the hasher — so it costs no
    /// second read.
    ///
    /// **Where these came from matters.** They were computed from the copies on
    /// this machine, not taken from a publisher: the mirrors are hand-written
    /// and one of them is a third-party upload, so there is no signed list to
    /// compare against. That pins *the bytes we have* and would not catch a
    /// mirror that served something wrong from the beginning. Better than a
    /// length, worth knowing the shape of.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// A model and everything needed to fetch it.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelSpec {
    /// Stable identifier.
    pub id: String,
    pub display_name: String,
    /// One sentence saying what this model is for, for a person choosing.
    ///
    /// In Chinese, unlike everything around it: this is user-facing copy, and
    /// the interface is Chinese throughout. The catalogue is where facts about
    /// models live, so the description belongs here rather than in whichever
    /// front-end happens to want it.
    ///
    /// Optional, and `#[serde(default)]`, so a catalogue written before this
    /// field existed still parses — the version gate is for incompatible
    /// shapes, and an absent optional field is not one.
    #[serde(default)]
    pub description: Option<String>,
    pub files: Vec<ModelFile>,
    /// Ids of models a run needs alongside this one.
    ///
    /// **The VAD is the case, and it was a release blocker.** Every
    /// transcription segments with Silero VAD, the pipeline looks for it at a
    /// fixed path (`Request::vad_for`), and it is not something a person
    /// chooses. It sat in this catalogue as a peer of the engines, so the
    /// window — which lists only what an engine can load — never offered it.
    /// A fresh install could download the default model, be told it was ready,
    /// and then fail on the first file with "VAD model not found".
    ///
    /// A field rather than a rule each front end applies, because two front
    /// ends applying it separately is how they end up disagreeing — which is
    /// this project's stated reason for keeping wire formats in one place.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Where to try, in order. The first success wins.
    pub mirrors: Vec<Mirror>,
}

/// A parsed catalogue.
#[derive(Debug, Clone, Deserialize)]
pub struct Catalog {
    pub version: u32,
    pub models: Vec<ModelSpec>,
}

impl Catalog {
    /// The catalogue built into this binary.
    pub fn embedded() -> Result<Self> {
        Self::from_json(EMBEDDED)
    }

    /// Parse a catalogue from JSON text.
    pub fn from_json(text: &str) -> Result<Self> {
        let catalog: Catalog = serde_json::from_str(text).map_err(|e| {
            Error::new(
                ErrorKind::Registry,
                format!("catalogue is not valid JSON: {e}"),
            )
        })?;
        catalog.validate()?;
        Ok(catalog)
    }

    /// Read a catalogue from a file.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            Error::new(
                ErrorKind::Io,
                format!("could not read catalogue {}: {e}", path.display()),
            )
        })?;
        Self::from_json(&text)
            .map_err(|e| Error::new(e.kind(), format!("{}: {}", path.display(), e.message())))
    }

    /// Read `path` if it exists, otherwise fall back to the embedded copy.
    ///
    /// A file that exists but does not parse is an error rather than a reason
    /// to fall back: silently ignoring a catalogue someone deliberately
    /// placed would hide their mistake until download time.
    pub fn load_or_embedded(path: &Path) -> Result<Self> {
        if path.is_file() {
            Self::from_file(path)
        } else {
            Self::embedded()
        }
    }

    /// Look a model up by id.
    pub fn find(&self, id: &str) -> Option<&ModelSpec> {
        self.models.iter().find(|spec| spec.id == id)
    }

    /// The models `spec` needs alongside itself.
    ///
    /// One level, not a transitive closure. The catalogue has exactly one
    /// requirement and it has none of its own; a chain or a cycle would mean
    /// models that cannot be fetched without each other, and would need a
    /// resolution order rather than a list.
    pub fn requirements(&self, spec: &ModelSpec) -> Vec<&ModelSpec> {
        spec.requires
            .iter()
            .filter_map(|id| self.find(id))
            .collect()
    }

    /// Whether `spec` and everything it needs are on disk.
    ///
    /// **The question a front end actually has before starting a job**, and
    /// the one that was being asked wrongly: asking only about `spec` is what
    /// let a fresh install be told its model was ready and then fail on the
    /// first file.
    pub fn all_present(&self, spec: &ModelSpec, root: &Path) -> bool {
        Downloader::is_present(spec, root)
            && self
                .requirements(spec)
                .iter()
                .all(|needed| Downloader::is_present(needed, root))
    }

    /// `spec` and whatever it needs that is not here yet, in fetch order.
    ///
    /// What a fetch should actually download. The model first, so a failure
    /// leaves the thing somebody asked for missing rather than something they
    /// never knew about.
    pub fn to_fetch<'a>(&'a self, spec: &'a ModelSpec, root: &Path) -> Vec<&'a ModelSpec> {
        let mut wanted = Vec::new();
        if !Downloader::is_present(spec, root) {
            wanted.push(spec);
        }
        for needed in self.requirements(spec) {
            if !Downloader::is_present(needed, root) {
                wanted.push(needed);
            }
        }
        wanted
    }

    /// Every model id, in file order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.models.iter().map(|spec| spec.id.as_str())
    }

    /// Reject a catalogue that would fail confusingly later.
    fn validate(&self) -> Result<()> {
        if self.version != SUPPORTED_VERSION {
            return Err(Error::new(
                ErrorKind::Registry,
                format!(
                    "catalogue version {} is not supported by this build (expects {SUPPORTED_VERSION})",
                    self.version
                ),
            ));
        }

        let mut seen = HashSet::new();
        for model in &self.models {
            if !seen.insert(model.id.as_str()) {
                return Err(Error::new(
                    ErrorKind::Registry,
                    format!("catalogue lists '{}' twice", model.id),
                ));
            }

            // Both of these would otherwise surface as a model that can never
            // be fetched, which is a worse failure than a refusal to start.
            if model.files.is_empty() {
                return Err(Error::new(
                    ErrorKind::Registry,
                    format!("'{}' lists no files", model.id),
                ));
            }
            if model.mirrors.is_empty() {
                return Err(Error::new(
                    ErrorKind::Registry,
                    format!("'{}' lists no mirrors", model.id),
                ));
            }

            // A requirement that names nothing is worse than a missing one:
            // `requirements` skips it, so the fetch silently does not happen
            // and the failure surfaces later as the model being "ready" and
            // then not working — which is the whole shape of the VAD bug this
            // field exists to fix.
            for needed in &model.requires {
                if needed == &model.id {
                    return Err(Error::new(
                        ErrorKind::Registry,
                        format!("'{}' requires itself", model.id),
                    ));
                }
                if self.find(needed).is_none() {
                    return Err(Error::new(
                        ErrorKind::Registry,
                        format!("'{}' requires '{needed}', which is not in the catalogue", model.id),
                    ));
                }
            }

            let mut locals = HashSet::new();
            for file in &model.files {
                if !locals.insert(file.local.as_str()) {
                    return Err(Error::new(
                        ErrorKind::Registry,
                        format!("'{}' uses '{}' for two files", model.id, file.local),
                    ));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_catalogue_parses() {
        let catalog = Catalog::embedded().expect("the shipped catalogue must be valid");
        assert_eq!(catalog.version, SUPPORTED_VERSION);
        assert!(catalog.find("sensevoice").is_some());
        assert!(catalog.find("silero-vad").is_some());
    }

    #[test]
    fn every_embedded_model_has_a_mirror_and_a_file() {
        let catalog = Catalog::embedded().unwrap();
        for model in &catalog.models {
            assert!(!model.mirrors.is_empty(), "{} has no mirrors", model.id);
            assert!(!model.files.is_empty(), "{} has no files", model.id);
        }
    }

    #[test]
    fn urls_join_without_doubling_slashes() {
        let mirror = Mirror {
            name: "test".into(),
            base_url: "https://example.com/".into(),
            repo: "/owner/repo/resolve/main".into(),
        };
        assert_eq!(
            mirror.url_for("model.onnx"),
            "https://example.com/owner/repo/resolve/main/model.onnx"
        );
    }

    #[test]
    fn a_newer_format_is_refused_rather_than_half_read() {
        let text = r#"{"version": 99, "models": []}"#;
        let err = Catalog::from_json(text).expect_err("must be refused");
        assert!(err.message().contains("99"), "got: {}", err.message());
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let text = r#"{
            "version": 1,
            "models": [
                {"id":"a","display_name":"A","files":[{"remote":"f","local":"f"}],
                 "mirrors":[{"name":"m","base_url":"https://x","repo":"r"}]},
                {"id":"a","display_name":"A again","files":[{"remote":"f","local":"f"}],
                 "mirrors":[{"name":"m","base_url":"https://x","repo":"r"}]}
            ]
        }"#;
        let err = Catalog::from_json(text).expect_err("must be refused");
        assert!(err.message().contains("twice"), "got: {}", err.message());
    }

    #[test]
    fn a_model_with_no_mirrors_is_refused() {
        let text = r#"{
            "version": 1,
            "models": [{"id":"a","display_name":"A","files":[{"remote":"f","local":"f"}],"mirrors":[]}]
        }"#;
        let err = Catalog::from_json(text).expect_err("must be refused");
        assert!(
            err.message().contains("no mirrors"),
            "got: {}",
            err.message()
        );
    }

    #[test]
    fn a_missing_size_is_allowed_and_reads_as_unknown() {
        let text = r#"{
            "version": 1,
            "models": [{"id":"a","display_name":"A","files":[{"remote":"f.bin","local":"f.bin"}],
                        "mirrors":[{"name":"m","base_url":"https://x","repo":"r"}]}]
        }"#;
        let catalog = Catalog::from_json(text).expect("size is optional");
        assert_eq!(catalog.find("a").unwrap().files[0].size, None);
    }

    #[test]
    fn malformed_json_names_the_problem() {
        let err = Catalog::from_json("{ not json }").expect_err("must fail");
        assert!(
            err.message().contains("not valid JSON"),
            "got: {}",
            err.message()
        );
    }

    #[test]
    fn a_file_that_exists_but_does_not_parse_is_an_error_not_a_fallback() {
        let path = std::env::temp_dir().join("verse-catalog-broken.json");
        std::fs::write(&path, "{ nope }").unwrap();

        let err = Catalog::load_or_embedded(&path)
            .expect_err("a deliberate file must not be silently ignored");
        assert!(
            err.message().contains("verse-catalog-broken.json"),
            "got: {}",
            err.message()
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_falls_back_to_the_embedded_copy() {
        let path = std::env::temp_dir().join("verse-catalog-does-not-exist.json");
        let _ = std::fs::remove_file(&path);

        let catalog = Catalog::load_or_embedded(&path).expect("falls back");
        assert!(catalog.find("sensevoice").is_some());
    }

    #[test]
    fn the_models_that_need_the_vad_say_so() {
        // The bug this pins, in one assertion. Every transcription segments
        // with Silero VAD; it is not an engine, so the window — which lists
        // only what an engine can load — has no card for it. Without this
        // field nothing downloads it, and a fresh install is told its model is
        // ready and then fails on the first file.
        let catalog = Catalog::embedded().unwrap();
        for id in ["sensevoice", "qwen3-asr"] {
            let spec = catalog.find(id).unwrap();
            assert!(
                spec.requires.iter().any(|r| r == "silero-vad"),
                "{id} needs the VAD and does not say so"
            );
        }

        // And the VAD needs nothing, which is why one level of resolution is
        // enough and `requirements` does not recurse.
        assert!(catalog.find("silero-vad").unwrap().requires.is_empty());
    }

    #[test]
    fn a_requirement_that_names_nothing_is_refused() {
        // `requirements` skips an id it cannot resolve, so a typo would not
        // fail here — it would silently not download, and surface later as a
        // model that says it is ready and does not work.
        let text = r#"{"version": 1, "models": [
            {"id": "a", "display_name": "A", "requires": ["b"],
             "files": [{"remote": "a.onnx", "local": "a.onnx"}],
             "mirrors": [{"name": "m", "base_url": "https://e.com", "repo": "r"}]}
        ]}"#;
        let err = Catalog::from_json(text).expect_err("must be refused");
        assert!(err.message().contains('b'), "got: {}", err.message());
    }

    #[test]
    fn a_model_that_requires_itself_is_refused() {
        let text = r#"{"version": 1, "models": [
            {"id": "a", "display_name": "A", "requires": ["a"],
             "files": [{"remote": "a.onnx", "local": "a.onnx"}],
             "mirrors": [{"name": "m", "base_url": "https://e.com", "repo": "r"}]}
        ]}"#;
        let err = Catalog::from_json(text).expect_err("must be refused");
        assert!(err.message().contains("itself"), "got: {}", err.message());
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("verse-catalog-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch");
        dir
    }

    /// Two models, where `main` needs `extra`.
    fn pair() -> Catalog {
        let text = r#"{"version": 1, "models": [
            {"id": "main", "display_name": "Main", "requires": ["extra"],
             "files": [{"remote": "m.onnx", "local": "m.onnx"}],
             "mirrors": [{"name": "m", "base_url": "https://e.com", "repo": "r"}]},
            {"id": "extra", "display_name": "Extra",
             "files": [{"remote": "e.onnx", "local": "e.onnx"}],
             "mirrors": [{"name": "m", "base_url": "https://e.com", "repo": "r"}]}
        ]}"#;
        Catalog::from_json(text).expect("valid")
    }

    fn place(spec: &ModelSpec, root: &Path, file: &str) {
        let dir = Downloader::directory_for(spec, root);
        std::fs::create_dir_all(&dir).expect("create model dir");
        std::fs::write(dir.join(file), b"").expect("write");
    }

    #[test]
    fn a_model_is_not_ready_until_what_it_needs_is_here() {
        // The state a fresh install reached and then called ready.
        let catalog = pair();
        let root = scratch("ready");
        let main = catalog.find("main").unwrap();
        let extra = catalog.find("extra").unwrap();

        place(main, &root, "m.onnx");
        assert!(
            !catalog.all_present(main, &root),
            "the model itself is here and the thing it needs is not"
        );

        let wanted: Vec<&str> = catalog
            .to_fetch(main, &root)
            .iter()
            .map(|spec| spec.id.as_str())
            .collect();
        assert_eq!(wanted, vec!["extra"], "only what is missing is fetched");

        place(extra, &root, "e.onnx");
        assert!(catalog.all_present(main, &root));
        assert!(catalog.to_fetch(main, &root).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_fetch_names_the_model_before_what_it_needs() {
        // The model first, so a failure leaves the thing somebody asked for
        // missing rather than something they never knew about.
        let catalog = pair();
        let root = scratch("order");
        let main = catalog.find("main").unwrap();

        let wanted: Vec<&str> = catalog
            .to_fetch(main, &root)
            .iter()
            .map(|spec| spec.id.as_str())
            .collect();
        assert_eq!(wanted, vec!["main", "extra"]);

        let _ = std::fs::remove_dir_all(&root);
    }
}
