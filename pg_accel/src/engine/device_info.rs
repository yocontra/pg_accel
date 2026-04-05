//! SQL-callable function exposing runtime device and configuration info.

use pgrx::prelude::*;

use super::cost::PlatformProfile;

/// Helper to extract a printable string from a fixed-size `c_char` buffer.
#[must_use]
pub fn cchar_buf_to_string(buf: &[std::ffi::c_char]) -> String {
    // SAFETY: the buffer is a fixed-size C string from our own fallback/bridge
    // layer, always null-terminated or fully zero-filled.
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Returns a single-row table with runtime device and configuration info.
///
/// Useful for diagnostics and verifying that GPU support is correctly detected.
#[pg_extern]
fn pg_accel_device_info() -> TableIterator<
    'static,
    (
        name!(cpu_cores, i32),
        name!(configured_workers, i32),
        name!(gpu_available, bool),
        name!(gpu_device_name, String),
        name!(memory_model, String),
        name!(pg_version, i32),
        name!(pg_accel_version, String),
    ),
> {
    let profile = PlatformProfile::detect();
    let configured_workers = 0_i32; // GPU-only mode, no CPU worker threads
    let device = crate::gpu::get_device_info();

    let gpu_device_name = cchar_buf_to_string(&device.device_name);
    let gpu_available = !gpu_device_name.is_empty() || device.compute_units > 0;

    let memory_model = if profile.unified_memory {
        "unified"
    } else if profile.has_gpu && !profile.unified_memory {
        "discrete"
    } else {
        "cpu_only"
    };

    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let cpu_cores = profile.cpu_cores as i32;

    // PG_VERSION_NUM is a compile-time constant provided by pgrx (e.g. 170000).
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let pg_version = pg_sys::PG_VERSION_NUM as i32;

    TableIterator::once((
        cpu_cores,
        configured_workers,
        gpu_available,
        gpu_device_name,
        memory_model.to_owned(),
        pg_version,
        env!("CARGO_PKG_VERSION").to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_returns_empty_string() {
        let buf: [std::ffi::c_char; 8] = [0; 8];
        assert_eq!(cchar_buf_to_string(&buf), "");
    }

    #[test]
    fn valid_ascii_string() {
        let buf: [std::ffi::c_char; 8] = [
            b'H' as std::ffi::c_char,
            b'e' as std::ffi::c_char,
            b'l' as std::ffi::c_char,
            b'l' as std::ffi::c_char,
            b'o' as std::ffi::c_char,
            0,
            0,
            0,
        ];
        assert_eq!(cchar_buf_to_string(&buf), "Hello");
    }

    #[test]
    fn no_null_terminator_returns_full_string() {
        let buf: [std::ffi::c_char; 4] = [
            b'A' as std::ffi::c_char,
            b'B' as std::ffi::c_char,
            b'C' as std::ffi::c_char,
            b'D' as std::ffi::c_char,
        ];
        assert_eq!(cchar_buf_to_string(&buf), "ABCD");
    }

    #[test]
    fn null_in_middle_truncates() {
        let buf: [std::ffi::c_char; 6] = [
            b'H' as std::ffi::c_char,
            b'i' as std::ffi::c_char,
            0,
            b'X' as std::ffi::c_char,
            b'Y' as std::ffi::c_char,
            0,
        ];
        assert_eq!(cchar_buf_to_string(&buf), "Hi");
    }

    #[test]
    fn single_char_buffer() {
        let buf: [std::ffi::c_char; 1] = [b'Z' as std::ffi::c_char];
        assert_eq!(cchar_buf_to_string(&buf), "Z");
    }

    #[test]
    fn single_null_byte() {
        let buf: [std::ffi::c_char; 1] = [0];
        assert_eq!(cchar_buf_to_string(&buf), "");
    }

    #[test]
    fn empty_slice() {
        let buf: [std::ffi::c_char; 0] = [];
        assert_eq!(cchar_buf_to_string(&buf), "");
    }

    #[test]
    fn leading_null_returns_empty() {
        let buf: [std::ffi::c_char; 4] = [0, b'A' as std::ffi::c_char, b'B' as std::ffi::c_char, 0];
        assert_eq!(cchar_buf_to_string(&buf), "");
    }
}
