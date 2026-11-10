use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("material".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("slint build failed");
    embed_flags();
    embed_build_id();
}

/// DUORAY_BUILD: the git commit this library is built from ("-dirty" with
/// uncommitted changes). Updates compare it with the manifest's, so a
/// re-uploaded build of the same commit is not offered again.
///
/// Cargo.lock does not count: cargo rewrites it during the build when
/// duoray-core next door (a path dependency) has a new version, which made
/// the 0.1.8 release "-dirty" and offered to itself as an update forever.
fn embed_build_id() {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let id = match git(&["rev-parse", "--short=12", "HEAD"]) {
        Some(commit) if git(&["status", "--porcelain", "--untracked-files=no", "--", ".", ":(exclude)Cargo.lock"]).is_some_and(|s| !s.is_empty()) => {
            format!("{commit}-dirty")
        }
        Some(commit) => commit,
        None => "unknown".into(),
    };
    println!("cargo:rustc-env=DUORAY_BUILD={id}");
    // A commit moves the branch ref, not HEAD itself.
    for path in [".git/HEAD", ".git/index", ".git/refs/heads", ".git/packed-refs"] {
        println!("cargo:rerun-if-changed={path}");
    }
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
