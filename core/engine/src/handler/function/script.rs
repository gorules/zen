use std::fmt::Debug;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::handler::function::vm::BASE_VM;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluateResponse {
    pub output: Value,
    pub log: Vec<Value>,
}

pub struct Script {
    isolate: v8::OwnedIsolate,
    timeout: Option<Duration>,
}

impl Script {
    pub fn new() -> Self {
        Self {
            isolate: v8::Isolate::new(
                v8::CreateParams::default().snapshot_blob(BASE_VM.as_slice()),
            ),
            timeout: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        assert!(timeout > Duration::ZERO);

        self.timeout = Some(timeout);
        self
    }

    pub async fn call<P>(&mut self, source: &str, args: &P) -> anyhow::Result<EvaluateResponse>
    where
        P: Serialize,
    {
        let handle = self.isolate.thread_safe_handle();

        let args_str =
            serde_json::to_string(args).context("Failed to serialize function arguments")?;

        let js_code_source = format!("const now = Date.now(); main({});", args_str);

        let handle_scope = &mut v8::HandleScope::new(&mut self.isolate);
        let context = v8::Context::new(handle_scope);
        let scope = &mut v8::ContextScope::new(handle_scope, context);
        let tc_scope = &mut v8::TryCatch::new(scope);

        let src = v8::String::new(tc_scope, source).context("Failed to compile source code")?;

        let js_src = v8::String::new(tc_scope, js_code_source.as_str())
            .context("Failed to compile source code")?;

        if let Some(timeout) = self.timeout {
            thread::spawn(move || {
                thread::sleep(timeout);
                handle.terminate_execution();
            });
        }

        let Some(src_script) = v8::Script::compile(tc_scope, src, None) else {
            let exception = tc_scope.exception().context("Failed to load script")?;
            return Err(anyhow!(exception.to_rust_string_lossy(tc_scope)));
        };

        if let None = src_script.run(tc_scope) {
            let exception = tc_scope.exception().unwrap();
            return Err(anyhow!(exception.to_rust_string_lossy(tc_scope)));
        }

        let Some(js_script) = v8::Script::compile(tc_scope, js_src, None) else {
            let exception = tc_scope.exception().context("Failed to load script")?;
            return Err(anyhow!(exception.to_rust_string_lossy(tc_scope)));
        };

        let Some(result) = js_script.run(tc_scope) else {
            if tc_scope.has_terminated() {
                return Err(anyhow!("Timeout exceeded"));
            }

            let exception = tc_scope
                .exception()
                .context("Failed to run loaded script")?;
            return Err(anyhow!(exception.to_rust_string_lossy(tc_scope)));
        };

        let result_string: String =
            serde_v8::from_v8(tc_scope, result).context("Failed to parse function result")?;

        serde_json::from_str(result_string.as_str()).context("Failed to parse function result")
    }
}
