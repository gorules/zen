use std::env;
use std::path::PathBuf;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

    cbindgen::Builder::new()
        .with_language(cbindgen::Language::C)
        .with_crate(crate_dir)
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file("zen_engine.h");

    #[cfg(feature = "csharp")]
    {
        let rust_files: Vec<PathBuf> = glob::glob("src/**/*.rs")
            .unwrap()
            .map(|s| s.unwrap())
            .collect();

        let valid_rust_files: Vec<PathBuf> = rust_files
            .into_iter()
            .filter(|p| !p.ends_with("languages/go.rs"))
            .collect();

        let mut cs_builder = csbindgen::Builder::default();
        for p in valid_rust_files.into_iter() {
            cs_builder = cs_builder.input_extern_file(p.to_str().unwrap().to_string())
        }

        cs_builder
            .csharp_dll_name("libzen_ffi")
            .csharp_namespace("GoRules.Zen")
            .csharp_class_name("ZenFfi")
            .generate_csharp_file("ZenFfi.g.cs")
            .unwrap();
    }
}
