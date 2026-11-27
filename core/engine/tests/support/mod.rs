pub mod columns;
#[allow(dead_code)]
pub mod golden;

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use zen_engine::loader::{FilesystemLoader, FilesystemLoaderOptions};
use zen_engine::model::{DecisionContent, GraphContent};

#[allow(dead_code)]
pub struct Clock;

#[allow(dead_code)]
impl Clock {
    fn cell() -> &'static std::sync::OnceLock<Option<zen_engine::context::Instant>> {
        static NOW: std::sync::OnceLock<Option<zen_engine::context::Instant>> =
            std::sync::OnceLock::new();
        &NOW
    }

    pub fn set(text: &str) {
        let _ = Self::cell().set(zen_engine::context::parse_instant(text));
    }

    pub fn now() -> Option<zen_engine::context::Instant> {
        Self::cell().get().copied().flatten()
    }

    pub fn options() -> zen_engine::EvaluationOptions {
        zen_engine::EvaluationOptions {
            now: Self::now(),
            ..Default::default()
        }
    }
}

#[allow(dead_code)]
pub struct Switches {
    pub typed: bool,
    pub census: bool,
}

#[allow(dead_code)]
pub fn switches() -> Switches {
    let switches = Switches {
        typed: std::env::var("ZEN_TYPED").map_or(true, |v| v.trim() != "0"),
        census: std::env::var("ZEN_CENSUS").is_ok(),
    };
    zen_engine::diagnostics::set_typed(switches.typed);
    zen_engine::diagnostics::set_census(switches.census);
    switches
}

#[allow(dead_code)]
pub fn test_data_root() -> String {
    let cargo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    cargo_root
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("test-data")
        .to_string_lossy()
        .to_string()
}

pub fn load_raw_test_data(key: &str) -> BufReader<File> {
    let file = File::open(Path::new(&test_data_root()).join(key)).unwrap();
    BufReader::new(file)
}

#[allow(dead_code)]
pub fn load_test_data(key: &str) -> GraphContent {
    let content: DecisionContent = serde_json::from_reader(load_raw_test_data(key)).unwrap();
    match content {
        DecisionContent::Graph(g) => (*g).clone(),
        DecisionContent::Policy(_) => {
            panic!("expected graph test fixture, got policy: {key}")
        }
    }
}

#[allow(dead_code)]
pub fn create_fs_loader() -> FilesystemLoader {
    FilesystemLoader::new(FilesystemLoaderOptions {
        root: test_data_root(),
    })
}

#[allow(dead_code)]
pub fn policy_error(
    error: &zen_engine::EvaluationError,
) -> Option<&zen_engine::policy::EvaluationError> {
    match error {
        zen_engine::EvaluationError::Policy(error) => Some(error),
        _ => None,
    }
}
