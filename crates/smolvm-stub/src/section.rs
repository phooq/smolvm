//! Mach-O section reading for embedded assets (macOS only).
//!
//! On macOS, single-file packed binaries store assets in the `__DATA,__smolvm`
//! section. This module provides functions to read from that section at runtime.

#[cfg(target_os = "macos")]
use smolvm_pack::format::{PackManifest, SectionHeader, SECTION_HEADER_SIZE};

/// Result of reading embedded section data.
#[cfg(target_os = "macos")]
pub struct EmbeddedData {
    /// The section header with sizes and checksum.
    pub header: SectionHeader,
    /// The manifest parsed from the section.
    pub manifest: PackManifest,
    /// Pointer to the start of compressed assets.
    pub assets_ptr: *const u8,
    /// Size of compressed assets.
    pub assets_size: usize,
}

/// Try to read embedded data from the `__DATA,__smolvm` section.
///
/// Returns `None` if:
/// - Not on macOS
/// - Section doesn't exist
/// - Section contains only placeholder data
#[cfg(target_os = "macos")]
pub fn read_embedded_section() -> Option<EmbeddedData> {
    use std::ffi::CStr;

    // External function to get section data
    extern "C" {
        fn getsectiondata(
            mhp: *const MachHeader64,
            segname: *const i8,
            sectname: *const i8,
            size: *mut usize,
        ) -> *const u8;
    }

    #[repr(C)]
    struct MachHeader64 {
        magic: u32,
        cputype: i32,
        cpusubtype: i32,
        filetype: u32,
        ncmds: u32,
        sizeofcmds: u32,
        flags: u32,
        reserved: u32,
    }

    // External: get the Mach-O header for the main executable
    extern "C" {
        fn _dyld_get_image_header(image_index: u32) -> *const MachHeader64;
    }

    unsafe {
        // Get the main executable header (index 0)
        let header = _dyld_get_image_header(0);
        if header.is_null() {
            return None;
        }

        // Get section data
        let segname = CStr::from_bytes_with_nul(b"__DATA\0").unwrap();
        let sectname = CStr::from_bytes_with_nul(b"__smolvm\0").unwrap();
        let mut size: usize = 0;

        let data_ptr = getsectiondata(
            header,
            segname.as_ptr(),
            sectname.as_ptr(),
            &mut size,
        );

        if data_ptr.is_null() || size < SECTION_HEADER_SIZE {
            return None;
        }

        // Check if this is just the placeholder
        let magic_bytes = std::slice::from_raw_parts(data_ptr, 8);
        if magic_bytes != smolvm_pack::SECTION_MAGIC {
            // Section exists but contains placeholder, not real data
            return None;
        }

        // Read section header
        let header_bytes = std::slice::from_raw_parts(data_ptr, SECTION_HEADER_SIZE);
        let section_header = match SectionHeader::from_bytes(header_bytes) {
            Ok(h) => h,
            Err(_) => return None,
        };

        // Validate sizes
        let expected_size = SECTION_HEADER_SIZE
            + section_header.manifest_size as usize
            + section_header.assets_size as usize;
        if size < expected_size {
            return None;
        }

        // Read manifest
        let manifest_start = data_ptr.add(SECTION_HEADER_SIZE);
        let manifest_bytes =
            std::slice::from_raw_parts(manifest_start, section_header.manifest_size as usize);
        let manifest = match PackManifest::from_json(manifest_bytes) {
            Ok(m) => m,
            Err(_) => return None,
        };

        // Calculate assets pointer
        let assets_ptr = manifest_start.add(section_header.manifest_size as usize);

        Some(EmbeddedData {
            header: section_header,
            manifest,
            assets_ptr,
            assets_size: section_header.assets_size as usize,
        })
    }
}

/// Placeholder for non-macOS platforms.
#[cfg(not(target_os = "macos"))]
pub fn read_embedded_section() -> Option<()> {
    None
}
