use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("material".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("slint build failed");
    embed_flags();
}

/// Generates `flag_svg(code)` over assets/flags/<iso>.svg (Twemoji, CC-BY 4.0).
fn embed_flags() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/flags");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("assets/flags")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "svg"))
        .collect();
    entries.sort();
    let mut src = String::from("pub fn flag_svg(code: &str) -> Option<&'static [u8]> {\n    match code {\n");
    for p in entries {
        let code = p.file_stem().unwrap().to_string_lossy().to_ascii_uppercase();
        writeln!(src, "        {code:?} => Some(include_bytes!({:?})),", p.display().to_string()).unwrap();
    }
    src.push_str("        _ => None,\n    }\n}\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("flags.rs");
    std::fs::write(out, src).unwrap();
}
