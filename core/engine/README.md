# ZEN Engine

ZEN Engine is business friendly Open-Source Business Rules Engine
(BRE) to execute decision models according to the GoRules JSON
Decision Model (JDM) standard. It is written in Rust and provides
native bindings for NodeJS and Python. ZEN Engine allows to load
and execute JSON Decision Model (JDM) from JSON files.

## Resources

[Documentation](https://gorules.io/docs/)

[Online Rules Engine Editor](https://editor.gorules.io/)

## Installation

Add the following to your Cargo.toml file:
```toml
[dependencies]
zen-engine = "0"
```

## Usage
To execute a simple decision using a Noop (default) loader you can use the code below.

```rust
use serde_json::json;
use zen_engine::DecisionEngine;
use zen_engine::model::DecisionContent;

async fn evaluate() {
    let decision_content: DecisionContent = serde_json::from_str(include_str!("jdm_graph.json")).unwrap();
    let engine = DecisionEngine::default();
    let decision = engine.create_decision(decision_content.into());
  
    let result = decision.evaluate(&json!({ "input": 12 })).await;
}
```

Alternatively, you may create decision indirectly without constructing the engine utilising
`Decision::from` function.

## Loaders
For more advanced use cases where you want to load multiple decisions and utilise graphs you
may use one of the following pre-made loaders:
- FilesystemLoader - with a given path as a root it tries to load a decision based on relative path
- MemoryLoader - works as a HashMap (key-value store)
- ClosureLoader - allows for definition of simple async callback function which takes key as a parameter
  and returns an `Arc<DecisionContent>` instance
- NoopLoader - (default) fails to load decision, allows for usage of create_decision
  (mostly existing for streamlining API across languages)

### Filesystem loader
Assuming that you have a folder with decision models (.json files) which is located under /app/decisions,
you may use FilesystemLoader in the following way:

```rust
use serde_json::json;
use zen_engine::DecisionEngine;
use zen_engine::loader::{FilesystemLoader, FilesystemLoaderOptions};

async fn evaluate() {
    let engine = DecisionEngine::new(FilesystemLoader::new(FilesystemLoaderOptions {
        keep_in_memory: true, // optionally, keep in memory for increase performance
        root: "/app/decisions"
    }));
    
    let context = json!({ "customer": { "joinedAt": "2022-01-01" } });
    // If you plan on using it multiple times, you may cache JDM for minor performance gains
    // In case of bindings (in other languages, this increase is much greater)
    {
        let promotion_decision = engine.get_decision("commercial/promotion.json").await.unwrap();
        let result = promotion_decision.evaluate(&context).await.unwrap();
    }
    
    // Or on demand
    {
        let result = engine.evaluate("commercial/promotion.json", &context).await.unwrap();
    }
}
```

### Custom loader
You may create a custom loader for zen engine by implementing `DecisionLoader` trait using async_trait crate.
Here's an example of how MemoryLoader has been implemented.

```rust
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResponse};
use zen_engine::model::DecisionContent;

#[derive(Debug, Default)]
pub struct MemoryLoader {
    memory_refs: RwLock<HashMap<String, Arc<DecisionContent>>>,
}

impl MemoryLoader {
    pub fn add<K, D>(&self, key: K, content: D)
        where
            K: Into<String>,
            D: Into<DecisionContent>,
    {
        let mut mref = self.memory_refs.write().unwrap();
        mref.insert(key.into(), Arc::new(content.into()));
    }
    pub fn get<K>(&self, key: K) -> Option<Arc<DecisionContent>>
        where
            K: AsRef<str>,
    {
        let mref = self.memory_refs.read().unwrap();
        mref.get(key.as_ref()).map(|r| r.clone())
    }
    pub fn remove<K>(&self, key: K) -> bool
        where
            K: AsRef<str>,
    {
        let mut mref = self.memory_refs.write().unwrap();
        mref.remove(key.as_ref()).is_some()
    }
}

#[async_trait]
impl DecisionLoader for MemoryLoader {
    async fn load(&self, key: &str) -> LoaderResponse {
        self.get(&key)
            .ok_or_else(|| LoaderError::NotFound(key.to_string()))
    }
}
```