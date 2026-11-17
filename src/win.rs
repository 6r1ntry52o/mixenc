//! The Windows calls mixenc needs.

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Globalization::{MB_ERR_INVALID_CHARS, MultiByteToWideChar};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};

/// Windows' mapping of one CP932 pair, or `None` where it has none.
pub fn cp932_pair(lead: u8, trail: u8) -> Option<u16> {
    let bytes = [lead, trail];
    let mut wide = [0u16; 2];
    // SAFETY: both buffers are valid for the lengths given.
    let written = unsafe { MultiByteToWideChar(932, MB_ERR_INVALID_CHARS, bytes.as_ptr(), 2, wide.as_mut_ptr(), 2) };
    (written == 1).then_some(wide[0])
}

/// `LongPathsEnabled` under `HKLM\SYSTEM\CurrentControlSet\Control\FileSystem`: `Some(true)` when
/// it is 1, `None` when it cannot be read.
pub fn long_paths_enabled() -> Option<bool> {
    let key = wide("SYSTEM\\CurrentControlSet\\Control\\FileSystem");
    let value = wide("LongPathsEnabled");
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the strings are NUL-terminated and `data` is a DWORD of `size` bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut data).cast(),
            &mut size,
        )
    };
    (status == ERROR_SUCCESS).then_some(data == 1)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
