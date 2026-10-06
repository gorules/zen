#[path = "support/policy_gen.rs"]
mod policy_gen;

use policy_gen::{Gen, Rng};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::policy::{EvaluateRequest, EvaluationError, EvaluationResult, PolicyWorkspace};
use zen_expression::Variable;

const FIXTURES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/policy/fixtures/");
const EVALUATION: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/policy/evaluation.toml");

#[derive(Deserialize)]
struct EvaluationFile {
    test: Vec<EvaluationCase>,
}

#[derive(Deserialize)]
struct EvaluationCase {
    name: String,
    policies: Vec<String>,
    input: toml::Value,
}

struct Differential {
    compared: usize,
    succeeded: usize,
    failures: Vec<String>,
    kinds: std::collections::BTreeMap<String, usize>,
}

impl Differential {
    fn shape(result: &Result<EvaluationResult, EvaluationError>) -> Result<Value, String> {
        match result {
            Ok(result) => Ok(result.output.clone().into()),
            Err(error) => Err(format!("{error:?}")),
        }
    }

    fn request(path: &str, input: &Value) -> EvaluateRequest {
        Self::with_goals(path, input, &[])
    }

    fn with_goals(path: &str, input: &Value, goals: &[&str]) -> EvaluateRequest {
        EvaluateRequest {
            policy_path: Arc::from(path),
            input: Variable::from(input.clone()),
            goals: goals.iter().map(|g| Arc::from(*g)).collect(),
            trace: false,
        }
    }

    fn leaves(value: &Value, prefix: &str, out: &mut Vec<String>) {
        let Value::Object(map) = value else {
            out.push(prefix.to_string());
            return;
        };
        for (key, child) in map {
            let path = match prefix.is_empty() {
                true => key.clone(),
                false => format!("{prefix}.{key}"),
            };
            Self::leaves(child, &path, out);
        }
    }

    fn computed(ws: &PolicyWorkspace, path: &str, inputs: &[Value]) -> Vec<String> {
        let mut out = Vec::new();
        for input in inputs {
            let Ok(result) = ws.evaluate_with_driver(&Self::request(path, input)) else {
                continue;
            };
            let (mut produced, mut given) = (Vec::new(), Vec::new());
            Self::leaves(&result.output.into(), "", &mut produced);
            Self::leaves(input, "", &mut given);
            out = produced.into_iter().filter(|p| !given.contains(p)).collect();
            break;
        }
        out
    }

    fn run(&mut self, label: &str, ws: &PolicyWorkspace, path: &str, inputs: &[Value]) {
        let computed = Self::computed(ws, path, inputs);
        let choices: Vec<Vec<&str>> = std::iter::once(Vec::new())
            .chain(computed.iter().map(|c| vec![c.as_str()]))
            .chain(computed.windows(2).map(|w| vec![w[0].as_str(), w[1].as_str()]))
            .collect();
        let goals = |i: usize| choices[(i * 7) % choices.len()].as_slice();
        let requests: Vec<EvaluateRequest> = inputs
            .iter()
            .enumerate()
            .map(|(i, input)| Self::with_goals(path, input, goals(i)))
            .collect();
        let batch = ws.evaluate_batch(&requests);
        for (i, (input, batched)) in inputs.iter().zip(&batch).enumerate() {
            let want = Self::shape(&ws.evaluate_with_driver(&Self::with_goals(path, input, goals(i))));
            let single = Self::shape(&ws.evaluate(&Self::with_goals(path, input, goals(i))));
            let batched = Self::shape(batched);
            self.compared += 1;
            self.succeeded += usize::from(want.is_ok());
            if let Err(error) = &want {
                let kind: String = error.chars().take_while(|c| c.is_alphanumeric()).collect();
                *self.kinds.entry(kind).or_default() += 1;
            }
            if single != want {
                self.failures.push(format!("{label} single {input}\n  driver   {want:?}\n  compiled {single:?}"));
            }
            if batched != want {
                self.failures.push(format!("{label} batch {input}\n  driver   {want:?}\n  compiled {batched:?}"));
            }
        }
    }

    fn variants(base: &Value) -> Vec<Value> {
        let mut out = vec![base.clone()];
        let Some(object) = base.as_object() else {
            return out;
        };
        for (key, value) in object {
            let mut with = |replacement: Option<Value>| {
                let mut copy = object.clone();
                match replacement {
                    Some(v) => copy.insert(key.clone(), v),
                    None => copy.remove(key),
                };
                out.push(Value::Object(copy));
            };
            with(None);
            with(Some(Value::Null));
            if let Value::Object(inner) = value {
                for (field, v) in inner {
                    let mut nested = inner.clone();
                    nested.insert(
                        field.clone(),
                        match v {
                            Value::Number(_) => json!("text"),
                            Value::String(_) => json!(7),
                            Value::Bool(b) => json!(!b),
                            Value::Array(_) => json!([]),
                            _ => Value::Null,
                        },
                    );
                    with(Some(Value::Object(nested)));
                }
            }
        }
        out
    }

    fn finish(self) {
        println!("compared {} inputs, {} evaluated without error, errors {:?}", self.compared, self.succeeded, self.kinds);
        assert!(
            self.failures.is_empty(),
            "{} mismatches:\n{}",
            self.failures.len(),
            self.failures.iter().take(15).cloned().collect::<Vec<_>>().join("\n")
        );
    }
}

#[test]
fn compiled_policies_match_the_driver_on_fixtures() {
    let raw = std::fs::read_to_string(EVALUATION).expect("evaluation.toml");
    let file: EvaluationFile = toml::from_str(&raw).expect("evaluation cases");
    let mut differential = Differential {
        compared: 0,
        succeeded: 0,
        failures: Vec::new(),
        kinds: Default::default(),
    };
    for case in &file.test {
        let mut ws = PolicyWorkspace::new();
        for path in &case.policies {
            let text = std::fs::read_to_string(format!("{FIXTURES_DIR}{path}")).expect("fixture");
            ws.set_policy(path.as_str(), serde_json::from_str(&text).expect("policy document"));
        }
        let input: Value = serde_json::to_value(&case.input).expect("input");
        differential.run(&case.name, &ws, &case.policies[0], &Differential::variants(&input));
    }
    differential.finish();
}

#[test]
fn compiled_policies_match_the_driver_on_generated_documents() {
    let seeds: u64 = std::env::var("POLICY_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    let mut differential = Differential {
        compared: 0,
        succeeded: 0,
        failures: Vec::new(),
        kinds: Default::default(),
    };
    for seed in 0..seeds {
        let document = Gen::document(seed);
        let mut ws = PolicyWorkspace::new();
        let Ok(parsed) = serde_json::from_value(document) else {
            continue;
        };
        ws.set_policy("generated.json", parsed);
        let mut rng = Rng(seed.wrapping_mul(7919) | 1);
        let inputs: Vec<Value> = (0..24)
            .map(|i| match i % 3 {
                0 => Gen::input(&mut rng),
                _ => Gen::valid_input(&mut rng),
            })
            .collect();
        differential.run(&format!("seed {seed}"), &ws, "generated.json", &inputs);
    }
    differential.finish();
}

#[test]
#[ignore]
fn compiled_policy_throughput() {
    let rows: usize = std::env::var("BENCH_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    let seeds: u64 = std::env::var("POLICY_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let time = |elapsed: std::time::Duration| elapsed.as_nanos() as f64 / rows as f64;
    let mut ratios: Vec<(f64, f64)> = Vec::new();
    for seed in 0..seeds {
        let mut ws = PolicyWorkspace::new();
        let Ok(parsed) = serde_json::from_value(Gen::document(seed)) else {
            continue;
        };
        ws.set_policy("generated.json", parsed);
        let mut rng = Rng(seed.wrapping_mul(7919) | 1);
        let requests: Vec<EvaluateRequest> = (0..rows)
            .map(|_| Differential::request("generated.json", &Gen::valid_input(&mut rng)))
            .collect();
        if let Ok(profile) = std::env::var("BENCH_PROFILE") {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(profile.parse().unwrap_or(1));
            while std::time::Instant::now() < until {
                match std::env::var("BENCH_MODE").as_deref() {
                    Ok("single") => requests.iter().for_each(|r| drop(ws.evaluate(r))),
                    _ => drop(ws.evaluate_batch(&requests)),
                }
            }
            continue;
        }
        let mut best = [f64::MAX; 3];
        for _ in 0..5 {
            let start = std::time::Instant::now();
            requests.iter().for_each(|r| drop(ws.evaluate_with_driver(r)));
            best[0] = best[0].min(time(start.elapsed()));
            let start = std::time::Instant::now();
            requests.iter().for_each(|r| drop(ws.evaluate(r)));
            best[1] = best[1].min(time(start.elapsed()));
            let start = std::time::Instant::now();
            drop(ws.evaluate_batch(&requests));
            best[2] = best[2].min(time(start.elapsed()));
        }
        println!("seed {seed:3} driver {:>8.0} single {:>8.0} batch {:>8.0}  {:.2}x {:.2}x", best[0], best[1], best[2], best[0] / best[1], best[0] / best[2]);
        ratios.push((best[0] / best[1], best[0] / best[2]));
    }
    let gm = |f: fn(&(f64, f64)) -> f64| (ratios.iter().map(|r| f(r).ln()).sum::<f64>() / ratios.len() as f64).exp();
    println!("single: geomean {:.2}x | batch: geomean {:.2}x", gm(|r| r.0), gm(|r| r.1));
}

#[tokio::test]
async fn engine_batch_matches_single_evaluations() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::model::{DecisionContent, PolicyContent};
    use zen_engine::{DecisionEngine, EvaluationOptions};

    let loader = Arc::new(MemoryLoader::default());
    let mut rng = Rng(11);
    let document = serde_json::from_value(Gen::document(3)).expect("policy document");
    loader.add("policy", DecisionContent::Policy(PolicyContent(Arc::new(document))));
    let graph: DecisionContent = serde_json::from_str(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-data/graphs/loan-approval.json"))
            .or_else(|_| std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-data/loan-approval.json")))
            .expect("graph fixture"),
    )
    .expect("graph content");
    loader.add("graph", graph);
    let contexts: Vec<Variable> = (0..40)
        .map(|i| match i % 2 {
            0 => Variable::from(Gen::valid_input(&mut rng)),
            _ => Variable::from(Gen::input(&mut rng)),
        })
        .collect();
    let engine = DecisionEngine::default().with_loader(loader);
    for compiled in [false, true] {
        if compiled {
            engine.compile();
        }
        for key in ["policy", "graph"] {
            let fresh: Vec<Variable> = contexts.iter().map(Variable::deep_clone).collect();
            let batch = engine.evaluate_batch(key, &fresh, EvaluationOptions::default()).await;
            assert_eq!(batch.len(), contexts.len());
            for (context, batched) in contexts.iter().zip(batch) {
                let single = engine
                    .evaluate_with_opts(key, context.deep_clone(), EvaluationOptions::default())
                    .await;
                let shape = |r: Result<zen_engine::DecisionGraphResponse, Box<zen_engine::EvaluationError>>| {
                    r.map(|r| r.result.to_value()).map_err(|e| e.to_string())
                };
                assert_eq!(shape(batched), shape(single), "{key} compiled={compiled} {context:?}");
            }
        }
    }
}
