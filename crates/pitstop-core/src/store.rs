//! Apps installed from the Microsoft Store (MSIX packages), on Windows. Their
//! files sit under `%ProgramFiles%\WindowsApps\<package full name>`, a folder
//! only Windows may list, so where a package is comes from Windows itself.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
use windows_sys::Win32::Storage::Packaging::Appx::{
    GetPackagePathByFullName, GetPackagesByPackageFamily,
};

/// The folder of each package of `family` (e.g. `OpenAI.Codex_2p2nqsd0c76g0`)
/// installed for the current user; none when it isn't installed.
pub fn package_roots(family: &str) -> Vec<PathBuf> {
    let family: Vec<u16> = family.encode_utf16().chain(Some(0)).collect();
    let (mut count, mut length) = (0u32, 0u32);
    // SAFETY: `family` is NUL-terminated; with no name array and no buffer
    // the call only says how many names there are and how long they are.
    let asked = unsafe {
        GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            null_mut(),
            &mut length,
            null_mut(),
        )
    };
    if asked != ERROR_INSUFFICIENT_BUFFER || count == 0 {
        return Vec::new();
    }
    let mut names = vec![null_mut(); count as usize];
    let mut buffer = vec![0u16; length as usize];
    // SAFETY: `names` holds `count` pointers and `buffer` `length` units, the
    // sizes the call above gave; Windows points each name into `buffer`.
    let got = unsafe {
        GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            names.as_mut_ptr(),
            &mut length,
            buffer.as_mut_ptr(),
        )
    };
    if got != ERROR_SUCCESS {
        return Vec::new();
    }
    names.truncate(count as usize);
    names
        .into_iter()
        .filter_map(|name| {
            let mut length = 0u32;
            // SAFETY: `name` is a NUL-terminated string in `buffer`, which
            // lives until the end of this function; with no path buffer the
            // call only says how long the path is.
            let asked = unsafe { GetPackagePathByFullName(name, &mut length, null_mut()) };
            if asked != ERROR_INSUFFICIENT_BUFFER || length == 0 {
                return None;
            }
            let mut path = vec![0u16; length as usize];
            // SAFETY: as above, with `path` holding the `length` units asked for.
            let got = unsafe { GetPackagePathByFullName(name, &mut length, path.as_mut_ptr()) };
            if got != ERROR_SUCCESS {
                return None;
            }
            let end = path
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(path.len());
            Some(PathBuf::from(OsString::from_wide(&path[..end])))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_family_nobody_installed_has_no_folder() {
        assert!(package_roots("Nobody.Nothing_0000000000000").is_empty());
        assert!(package_roots("").is_empty());
    }
}
