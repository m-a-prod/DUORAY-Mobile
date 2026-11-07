//! The phone UI in a desktop window (see src/platform/preview.rs).

#[cfg(not(target_os = "android"))]
fn main() -> anyhow::Result<()> {
    duoray_mobile::preview()
}

#[cfg(target_os = "android")]
fn main() {}
