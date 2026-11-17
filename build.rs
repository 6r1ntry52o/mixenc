//! Embeds the application manifest that declares mixenc long path aware, so that Windows does not
//! cut its file operations at 260 characters where long paths are enabled. The file operations add
//! `\?\` themselves as well (Rust's standard library does), so they work where they are not.

use embed_manifest::manifest::{ExecutionLevel, Setting, SupportedOS};
use embed_manifest::{embed_manifest, empty_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest = empty_manifest()
            .name("mixenc")
            .supported_os(SupportedOS::Windows10..=SupportedOS::Windows10)
            .long_path_aware(Setting::Enabled)
            .requested_execution_level(ExecutionLevel::AsInvoker);
        embed_manifest(manifest).expect("embedding the application manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
