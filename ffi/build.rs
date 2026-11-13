//! Embeds the Windows version resource into `league_link.dll`.
//!
//! SignPath's artifact configuration enforces file metadata restrictions:
//! every signed binary must carry the project's product name and a product
//! version that is identical across all files of one build. Rust does not
//! emit a PE version resource by default, so we compile one here from the
//! crate version (which the release checklist keeps in sync with the tag).

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");

    // `cfg(windows)` in a build script refers to the host. The project's CI
    // and release pipeline run on windows-latest only, where host == target.
    #[cfg(windows)]
    {
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_owned());
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "league-link");
        res.set("FileDescription", "league-link — LCU API component (C ABI)");
        res.set("FileVersion", &version);
        res.set("ProductVersion", &version);
        res.set("LegalCopyright", "Copyright (c) QAQTam. MIT License.");
        res.set("OriginalFilename", "league_link.dll");
        res.set("InternalName", "league_link");
        // Build scripts are tooling, not library code: a hard failure here
        // must stop the build rather than ship an unversioned binary.
        res.compile()
            .expect("failed to compile the Windows version resource");
    }
}
