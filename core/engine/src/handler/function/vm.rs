use std::collections::HashMap;
use std::env;

use once_cell::sync::Lazy;
use v8::FunctionCodeHandling;

const EXPOSED_PREFIX: &'static str = "ZEN_EXPOSED_";

pub static BASE_VM: Lazy<Vec<u8>> = Lazy::new(|| {
    let platform = v8::new_default_platform(0, false).make_shared();
    v8::V8::initialize_platform(platform);
    v8::V8::initialize();

    let env_src_string = {
        let mut env_vars = HashMap::new();
        env::vars().for_each(|(key, value)| {
            if let Some(mod_key) = key.strip_prefix(EXPOSED_PREFIX) {
                env_vars.insert(mod_key.to_string(), value);
            }
        });

        format!(
            "const __GLOBAL__ENV = {}",
            serde_json::to_string(&env_vars).unwrap()
        )
    };

    let mut isolate = v8::Isolate::snapshot_creator(Default::default());

    {
        let handle_scope = &mut v8::HandleScope::new(&mut isolate);
        let context = v8::Context::new(handle_scope);

        let scope = &mut v8::ContextScope::new(handle_scope, context);

        let dayjs_src = v8::String::new(scope, include_str!("scripts/dayjs.js")).unwrap();
        let internal_src = v8::String::new(scope, include_str!("scripts/internals.js")).unwrap();
        let env_src = v8::String::new(scope, env_src_string.as_str()).unwrap();

        v8::Script::compile(scope, dayjs_src, None)
            .unwrap()
            .run(scope);
        v8::Script::compile(scope, internal_src, None)
            .unwrap()
            .run(scope);
        v8::Script::compile(scope, env_src, None)
            .unwrap()
            .run(scope);

        scope.set_default_context(context);
    }

    isolate
        .create_blob(FunctionCodeHandling::Keep)
        .unwrap()
        .to_vec()
});
