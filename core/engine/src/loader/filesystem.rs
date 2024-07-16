use std::collections::HashMap;
use std::fs::File;
use std::future::Future;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::loader::{DecisionLoader, LoaderError, LoaderResponse};
use crate::model::DecisionContent;

/// Loads decisions based on filesystem root
#[derive(Debug)]
pub struct FilesystemLoader {
    root: String,
    memory_refs: Option<RwLock<HashMap<String, Arc<DecisionContent>>>>,
}

#[derive(Serialize, Deserialize)]
pub struct FilesystemLoaderOptions<R: Into<String>> {
    pub root: R,
    pub keep_in_memory: bool,
}

impl FilesystemLoader {
    pub fn new<R>(options: FilesystemLoaderOptions<R>) -> Self
    where
        R: Into<String>,
    {
        let root = options.root.into();
        let memory_refs = if options.keep_in_memory {
            Some(Default::default())
        } else {
            None
        };

        Self { root, memory_refs }
    }

    fn key_to_path<K: AsRef<str>>(&self, key: K) -> PathBuf {
        Path::new(&self.root).join(key.as_ref())
    }

    async fn read_from_file<K>(&self, key: K) -> LoaderResponse
    where
        K: AsRef<str>,
    {
        if let Some(memory_refs) = &self.memory_refs {
            let mref = memory_refs.read().await;
            if let Some(decision_content) = mref.get(key.as_ref()) {
                return Ok(decision_content.clone());
            }
        }

        let path = self.key_to_path(key.as_ref());
        if !Path::exists(&path) {
            return Err(LoaderError::NotFound(String::from(key.as_ref())).into());
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

        let ptr = Arc::new(result);
        if let Some(memory_refs) = &self.memory_refs {
            let mut mref = memory_refs.write().await;
            mref.insert(key.as_ref().to_string(), ptr.clone());
        }

        Ok(ptr)
    }
}

impl DecisionLoader for FilesystemLoader {
    fn load<'a>(&'a self, key: &'a str) -> impl Future<Output = LoaderResponse> + 'a {
        async move { self.read_from_file(key).await }
    }
}
