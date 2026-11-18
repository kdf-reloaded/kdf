//! Free space of the filesystem that holds a path (CRD ch.39 R39.8.0bc).
//!
//! Used before the backup of a shielded wallet database that is about to be migrated. The figure
//! is the space available to the calling user, not the raw free space of the volume.

use std::path::Path;

/// Bytes available to this process on the filesystem that holds `path`. `path` must exist.
///
/// # Errors
///
/// Returns the operating-system error text when the figure cannot be read, and on a platform
/// that is neither Unix nor Windows.
pub(crate) fn available_bytes(path: &Path) -> Result<u64, String> { imp::available_bytes(path) }

#[cfg(unix)]
mod imp {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub(super) fn available_bytes(path: &Path) -> Result<u64, String> {
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| format!("path {} contains a NUL byte", path.display()))?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: `c_path` is a valid NUL-terminated string and `stat` is a valid, writable
        // `statvfs` buffer; it is read only after the call reports success.
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
        if rc != 0 {
            return Err(format!(
                "statvfs failed for {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: `statvfs` returned 0, so it filled the buffer.
        let stat = unsafe { stat.assume_init() };
        // The field widths differ per platform.
        #[allow(clippy::unnecessary_cast)]
        let (blocks, fragment_size, block_size) = (stat.f_bavail as u64, stat.f_frsize as u64, stat.f_bsize as u64);
        // `f_frsize` is the unit of `f_bavail`; some filesystems leave it zero, and then `f_bsize` is.
        let unit = if fragment_size == 0 { block_size } else { fragment_size };
        Ok(blocks.saturating_mul(unit))
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use winapi::shared::ntdef::ULARGE_INTEGER;
    use winapi::um::fileapi::GetDiskFreeSpaceExW;

    pub(super) fn available_bytes(path: &Path) -> Result<u64, String> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        // A UNC directory name (`\\server\share\dir`) must end with a backslash for this call.
        let is_unc = wide.starts_with(&[u16::from(b'\\'), u16::from(b'\\')]);
        if is_unc && wide.last() != Some(&u16::from(b'\\')) {
            wide.push(u16::from(b'\\'));
        }
        wide.push(0);
        // SAFETY: an all-zero `ULARGE_INTEGER` is a valid value.
        let mut available: ULARGE_INTEGER = unsafe { std::mem::zeroed() };
        // SAFETY: `wide` is a NUL-terminated UTF-16 string and `available` is a valid out-pointer;
        // the two other out-pointers are optional and passed as null.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(format!(
                "GetDiskFreeSpaceExW failed for {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: the call succeeded, so the union holds the 64-bit quantity.
        Ok(unsafe { *available.QuadPart() })
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::path::Path;

    pub(super) fn available_bytes(_path: &Path) -> Result<u64, String> {
        Err("free-space query is not implemented for this platform".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_bytes_of_the_temp_dir_is_readable() {
        let available = available_bytes(&std::env::temp_dir()).unwrap();
        assert!(available > 0);
    }

    #[test]
    fn available_bytes_of_a_missing_path_is_an_error() {
        let missing = std::env::temp_dir().join("kdf-zcoin-no-such-dir-for-free-space");
        assert!(available_bytes(&missing).is_err());
    }
}
