use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::loader::{DynamicLoader, FilesystemLoader, FilesystemLoaderOptions, MemoryLoader};
use crate::model::DecisionContent;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum LoaderConfig {
    Static {
        content: HashMap<String, DecisionContent>,
    },
    #[serde(rename = "fs")]
    Filesystem {
        path: String,
    },
    Zip {
        bytes: Vec<u8>,
    },
}

impl LoaderConfig {
    pub fn into_loader(self) -> anyhow::Result<DynamicLoader> {
        match self {
            LoaderConfig::Static { content } => {
                let loader = MemoryLoader::default();
                for (key, decision_content) in content {
                    loader.add(key, decision_content);
                }
                Ok(Arc::new(loader))
            }
            LoaderConfig::Filesystem { path } => {
                Ok(Arc::new(FilesystemLoader::new(FilesystemLoaderOptions {
                    root: path,
                })))
            }
            LoaderConfig::Zip { bytes } => Self::loader_from_zip(&bytes),
        }
    }

    fn loader_from_zip(bytes: &[u8]) -> anyhow::Result<DynamicLoader> {
        use std::io::Read;

        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        let loader = MemoryLoader::default();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            if !entry.is_file() || !entry.name().ends_with(".json") {
                continue;
            }

            let key = entry.name().to_string();
            let mut buffer = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buffer)?;
            let content: DecisionContent = serde_json::from_slice(&buffer)?;
            loader.add(key, content);
        }

        Ok(Arc::new(loader))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRAPH_JSON: &str = r#"{"nodes":[],"edges":[]}"#;

    #[tokio::test]
    async fn static_config_serves_decisions_by_key() {
        let mut content = HashMap::new();
        content.insert(
            "graph.json".to_string(),
            serde_json::from_str::<DecisionContent>(GRAPH_JSON).unwrap(),
        );

        let loader = LoaderConfig::Static { content }.into_loader().unwrap();
        assert!(loader.load("graph.json").await.is_ok());
        assert!(loader.load("missing.json").await.is_err());
    }

    #[test]
    fn fs_config_reads_path() {
        let config: LoaderConfig = serde_json::from_str(r#"{"type":"fs","path":"p"}"#).unwrap();

        let LoaderConfig::Filesystem { path } = config else {
            panic!("expected filesystem loader config");
        };

        assert_eq!(path, "p");
    }

    #[tokio::test]
    async fn zip_config_decompresses_and_serves_decisions() {
        use std::io::Write;

        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file("graph.json", options).unwrap();
            writer.write_all(GRAPH_JSON.as_bytes()).unwrap();
            writer.finish().unwrap();
        }

        let loader = LoaderConfig::Zip {
            bytes: cursor.into_inner(),
        }
        .into_loader()
        .unwrap();

        assert!(loader.load("graph.json").await.is_ok());
        assert!(loader.load("missing.json").await.is_err());
    }
}
