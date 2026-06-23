use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use zen_engine::loader::{FilesystemLoader, FilesystemLoaderOptions};
use zen_engine::model::{DecisionContent, GraphContent};

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
        DecisionContent::Graph(g) => g,
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
