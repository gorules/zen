use std::fs::File;
use std::future::Future;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::loader::{DecisionLoader, LoaderError, LoaderResponse};
use crate::model::DecisionContent;

/// Loads decisions based on filesystem root
#[derive(Debug)]
pub struct FilesystemLoader {
    root: String,
}

#[derive(Serialize, Deserialize)]
pub struct FilesystemLoaderOptions<R: Into<String>> {
    pub root: R,
}

impl FilesystemLoader {
    pub fn new<R>(options: FilesystemLoaderOptions<R>) -> Self
    where
        R: Into<String>,
    {
        Self {
            root: options.root.into(),
        }
    }

    /// The file of a key: as named, else with `.json` (BRMS names documents
    /// and their imports without an extension, `models/cards`, while a
    /// folder on disk usually has `models/cards.json`).
    /// A folder of the same name (`models/cards/`) is never the file.
    fn key_to_path<K: AsRef<str>>(&self, key: K) -> PathBuf {
        let path = Path::new(&self.root).join(key.as_ref());
        // A dotted name (`fraud.rules`) is a name, not an extension.
        if path.is_file() || key.as_ref().ends_with(".json") {
            return path;
        }
        let with_json = Path::new(&self.root).join(format!("{}.json", key.as_ref()));
        if with_json.is_file() {
            with_json
        } else {
            path
        }
    }

    fn read_content<K: AsRef<str>>(&self, key: K) -> LoaderResponse {
        let path = self.key_to_path(key.as_ref());
        if !path.is_file() {
            return Err(LoaderError::NotFound(String::from(key.as_ref())));
        }

        let file = File::open(path).map_err(|e| LoaderError::Internal {
            key: String::from(key.as_ref()),
            source: e.into(),
        })?;

        let reader = BufReader::new(file);
        let result: DecisionContent =
            serde_json::from_reader(reader).map_err(|e| LoaderError::Internal {
                key: String::from(key.as_ref()),
                source: e.into(),
            })?;

        Ok(Arc::new(result))
    }
}

impl DecisionLoader for FilesystemLoader {
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = LoaderResponse> + 'a + Send>> {
        Box::pin(async move { self.read_content(key) })
    }

    fn load_sync(&self, key: &str) -> Option<LoaderResponse> {
        Some(self.read_content(key))
    }

    fn keys(&self) -> Option<Vec<Arc<str>>> {
        let root = Path::new(&self.root);
        let mut keys = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    let key = path.strip_prefix(root).ok().and_then(|rel| {
                        rel.components()
                            .map(|component| component.as_os_str().to_str())
                            .collect::<Option<Vec<_>>>()
                            .map(|segments| segments.join("/"))
                    });
                    if let Some(key) = key {
                        keys.push(Arc::from(key));
                    }
                }
            }
        }
        Some(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_loader() -> FilesystemLoader {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("test-data");
        FilesystemLoader::new(FilesystemLoaderOptions {
            root: root.to_string_lossy().to_string(),
        })
    }

    #[tokio::test]
    async fn load_and_load_sync_resolve_existing_key() {
        let loader = test_loader();

        assert!(loader.load("table.json").await.is_ok());
        assert!(loader.load_sync("table.json").unwrap().is_ok());
        // A key without its extension, as BRMS imports name documents.
        assert!(loader.load("table").await.is_ok());
    }

    #[test]
    fn key_resolves_to_the_file_beside_a_folder_of_the_same_name() {
        let root = std::env::temp_dir().join(format!("zen-fs-loader-{}", std::process::id()));
        std::fs::create_dir_all(root.join("models/cards")).unwrap();
        std::fs::write(root.join("models/cards.json"), "{}").unwrap();
        let loader = FilesystemLoader::new(FilesystemLoaderOptions {
            root: root.to_string_lossy().to_string(),
        });

        assert_eq!(
            loader.key_to_path("models/cards"),
            root.join("models/cards.json")
        );
        // A dotted name is a name: `<key>.json` is still tried.
        std::fs::write(root.join("models/rules.v2.json"), "{}").unwrap();
        assert_eq!(
            loader.key_to_path("models/rules.v2"),
            root.join("models/rules.v2.json")
        );
        // A key naming its `.json` is the file as named.
        assert_eq!(
            loader.key_to_path("models/cards.json"),
            root.join("models/cards.json")
        );
        assert!(loader.load_sync("models").unwrap().is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn load_reports_missing_key() {
        let loader = test_loader();

        assert!(loader.load("missing.json").await.is_err());
        assert!(loader.load_sync("missing.json").unwrap().is_err());
    }
}
