//! Build script for smolvm-stub.
//!
//! The stub uses dlopen to load libkrun dynamically at runtime after
//! assets are extracted, so no compile-time linking is required.
//!
//! On macOS, we create a placeholder section (__DATA,__smolvm) that can be
//! filled with assets at pack time, enabling proper code signing.

fn main() {
    // No compile-time linking required - libkrun is loaded via dlopen
    // after assets are extracted to the cache directory.

    // On macOS, add a placeholder section for embedded assets
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;

        // Create a small placeholder file
        let out_dir = std::env::var("OUT_DIR").unwrap();
        let placeholder_path = format!("{}/smolvm_placeholder.bin", out_dir);

        // Create placeholder with a marker we can find
        let mut f = std::fs::File::create(&placeholder_path).unwrap();
        // Write a recognizable pattern that won't appear naturally
        f.write_all(b"SMOLVM_SECTION_PLACEHOLDER_V1").unwrap();
        // Pad to reasonable size (the section will be expanded at pack time)
        f.write_all(&[0u8; 4]).unwrap();

        // Tell the linker to create a section with this content
        println!("cargo:rustc-link-arg=-Wl,-sectcreate,__DATA,__smolvm,{}", placeholder_path);
    }
}
