use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::Instant;
use zen_engine::loader::{FilesystemLoader, FilesystemLoaderOptions};
use zen_engine::DecisionContent;

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
pub fn load_test_data(key: &str) -> DecisionContent {
    serde_json::from_reader(load_raw_test_data(key)).unwrap()
}

#[allow(dead_code)]
pub fn create_fs_loader() -> FilesystemLoader {
    FilesystemLoader::new(FilesystemLoaderOptions {
        keep_in_memory: false,
        root: test_data_root(),
    })
}

#[allow(dead_code)]
fn format_with_underscores(n: u32) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push('_');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

#[allow(dead_code)]
pub fn benchmark<F, T>(name: &str, iterations: u32, prev_dur: Option<u128>, print: bool, mut func: F) -> (T, u128)
where
    F: FnMut() -> T,
{
    let start = Instant::now();
    let mut result = None;

    for _ in 0..iterations {
        result = Some(func());
    }

    let duration = start.elapsed();
    if print {
        println!("=== {} ===", name);
        println!("Iterations: {}", format_with_underscores(iterations));
        println!("Average: **{:?}**", duration / iterations);
        println!("Ops/sec: {:.0}", iterations as f64 / duration.as_secs_f64());
    }

    if let Some(prev_dur) = prev_dur {
        let c_dur = duration.as_micros();
        let diff = prev_dur as f64 - c_dur as f64;
        println!("Faster: **{:.1}%**",(diff/prev_dur as f64) * 100.0);
    }
    (result.unwrap(), duration.as_micros())
}