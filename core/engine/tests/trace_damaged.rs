use std::panic::{catch_unwind, AssertUnwindSafe};
use zen_engine::trace::testing::{parse, restore_val, Val};

const ROUNDS: usize = 16;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn count(v: &Val) -> usize {
    1 + match v {
        Val::Arr(a) => a.iter().map(count).sum(),
        Val::Obj(o) => o.values().map(count).sum(),
        _ => 0,
    }
}

fn replace(v: &Val, target: &mut usize, with: &Val) -> Val {
    if *target == 0 {
        *target = usize::MAX;
        return with.clone();
    }
    if *target != usize::MAX {
        *target -= 1;
    }
    match v {
        Val::Arr(a) => Val::arr(a.iter().map(|x| replace(x, target, with)).collect()),
        Val::Obj(o) => {
            let mut out = zen_engine::trace::testing::Obj::new();
            for (k, x) in o.iter() {
                out.insert(k.clone(), replace(x, target, with));
            }
            Val::obj(out)
        }
        _ => v.clone(),
    }
}

#[test]
fn damaged_documents_are_refused_without_panicking() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/trace/poc.jsonl"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let swaps: Vec<Val> = [
        "null",
        "0",
        "-1",
        "1.5",
        "\"x\"",
        "\"/a/0\"",
        "[]",
        "{}",
        "[0,1]",
        "{\"$ref\":\"self/nodes\"}",
        "{\"$runs\":[[1,5,1]]}",
        "100000000000",
    ]
    .iter()
    .map(|s| parse(s).unwrap())
    .collect();
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let mut tried = 0;
    for line in text.lines().step_by(3) {
        let f = parse(line).unwrap();
        let request = f.get("request").cloned().unwrap_or(Val::Null);
        let docs = f.get("documents").cloned();
        let doc = parse(f.get("poc").unwrap().as_str().unwrap()).unwrap();
        let n = count(&doc);
        for _ in 0..ROUNDS {
            let mut at = (rng.next() as usize) % n;
            let with = &swaps[(rng.next() as usize) % swaps.len()];
            let damaged = replace(&doc, &mut at, with).to_json();
            let r = catch_unwind(AssertUnwindSafe(|| {
                restore_val(&damaged, &request, docs.as_ref()).map(|_| ())
            }));
            assert!(r.is_ok(), "panic on {}", &damaged[..damaged.len().min(400)]);
            tried += 1;
        }
    }
    assert!(tried > 1000);
}
