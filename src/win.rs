//! The Windows calls mixenc needs.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Globalization::{
    MB_ERR_INVALID_CHARS, MultiByteToWideChar, WC_NO_BEST_FIT_CHARS, WideCharToMultiByte,
};
use windows_sys::Win32::Storage::FileSystem::{
    FindClose, FindFirstFileW, GetLongPathNameW, GetShortPathNameW, WIN32_FIND_DATAW,
};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};

/// Windows' mapping of one CP932 pair, or `None` where it has none.
pub fn cp932_pair(lead: u8, trail: u8) -> Option<u16> {
    let bytes = [lead, trail];
    let mut wide = [0u16; 2];
    // SAFETY: both buffers are valid for the lengths given.
    let written = unsafe { MultiByteToWideChar(932, MB_ERR_INVALID_CHARS, bytes.as_ptr(), 2, wide.as_mut_ptr(), 2) };
    (written == 1).then_some(wide[0])
}

/// `text` in CP932 as Windows writes it (as Visual Studio saves a file: `Ⅰ` as NEC's `87 54`), or
/// `None` when a character has no CP932 form. No best-fit character stands in (Windows would
/// write a backslash for `¥`).
pub fn cp932_encode(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() {
        return Some(Vec::new());
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    let len = i32::try_from(wide.len()).ok()?;
    let flags = WC_NO_BEST_FIT_CHARS;
    // SAFETY: `wide` is valid for `len` units; a null output asks for the size.
    let size = unsafe {
        WideCharToMultiByte(
            932,
            flags,
            wide.as_ptr(),
            len,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    };
    if size <= 0 {
        return None;
    }
    let mut out = vec![0u8; size as usize];
    let mut used_default = 0;
    // SAFETY: `out` has room for `size` bytes and `used_default` is a valid BOOL.
    let written = unsafe {
        WideCharToMultiByte(932, flags, wide.as_ptr(), len, out.as_mut_ptr(), size, std::ptr::null(), &mut used_default)
    };
    (written == size && used_default == 0).then_some(out)
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

/// `path` with its 8.3 short names (`RUNNER~1`) written in full and its parts in the disk's case,
/// without following links or junctions; `None` when it does not exist.
pub fn long_path_name(path: &Path) -> Option<PathBuf> {
    path_name(path, GetLongPathNameW)
}

/// `path` with 8.3 short names where the volume has them; `None` when it does not exist.
pub fn short_path_name(path: &Path) -> Option<PathBuf> {
    path_name(path, GetShortPathNameW)
}

/// The name of the last part of `path` as its directory lists it (its case on disk).
pub fn find_name(path: &Path) -> Option<OsString> {
    let input = wide_path(path);
    // SAFETY: WIN32_FIND_DATAW is plain data; zero is a valid value.
    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    // SAFETY: `input` is NUL-terminated and `data` is a valid out-parameter.
    let handle = unsafe { FindFirstFileW(input.as_ptr(), &mut data) };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    // SAFETY: `handle` came from FindFirstFileW.
    unsafe { FindClose(handle) };
    let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
    Some(OsString::from_wide(&data.cFileName[..len]))
}

/// The path NUL-terminated for a wide Windows call, with `\\?\` when it is long.
fn wide_path(path: &Path) -> Vec<u16> {
    let s = path.as_os_str();
    let verbatim = s.len() >= 240 && !s.to_string_lossy().starts_with(r"\\");
    let mut input: Vec<u16> = if verbatim { OsStr::new(r"\\?\").encode_wide().collect() } else { Vec::new() };
    input.extend(s.encode_wide());
    input.push(0);
    input
}

fn path_name(path: &Path, convert: unsafe extern "system" fn(*const u16, *mut u16, u32) -> u32) -> Option<PathBuf> {
    let input = wide_path(path);
    let mut buffer = vec![0u16; 260];
    loop {
        // SAFETY: `input` is NUL-terminated and `buffer` has room for `buffer.len()` units.
        let len = unsafe { convert(input.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        match len {
            0 => return None,
            // Too small: `len` is the size needed, with the NUL.
            len if len > buffer.len() => buffer.resize(len, 0),
            len => return Some(PathBuf::from(OsString::from_wide(&buffer[..len]))),
        }
    }
}
