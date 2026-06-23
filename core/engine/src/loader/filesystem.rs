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

    fn key_to_path<K: AsRef<str>>(&self, key: K) -> PathBuf {
        Path::new(&self.root).join(key.as_ref())
    }

    fn read_content<K: AsRef<str>>(&self, key: K) -> LoaderResponse {
        let path = self.key_to_path(key.as_ref());
        if !Path::exists(&path) {
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
    }

    #[tokio::test]
    async fn load_reports_missing_key() {
        let loader = test_loader();

        assert!(loader.load("missing.json").await.is_err());
        assert!(loader.load_sync("missing.json").unwrap().is_err());
    }
}
