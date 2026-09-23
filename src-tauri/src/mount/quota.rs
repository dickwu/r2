//! Admission accounting includes a stage and its immutable upload snapshot.
use std::{collections::HashMap, path::Path};

pub struct StageQuota {
    pub limit: u64,
    used: u64,
    files: HashMap<u64, u64>,
}
impl Default for StageQuota {
    fn default() -> Self {
        Self {
            limit: u64::MAX,
            used: 0,
            files: HashMap::new(),
        }
    }
}
impl StageQuota {
    pub fn reserve(&mut self, id: u64, bytes: u64) -> bool {
        let previous = self.files.get(&id).copied().unwrap_or(0);
        let proposed = self.used.saturating_sub(previous).checked_add(bytes);
        if proposed.is_none_or(|size| size > self.limit) {
            return false;
        }
        self.used = proposed.unwrap();
        self.files.insert(id, bytes);
        true
    }
    pub fn restore(&mut self, id: u64, bytes: u64) {
        let previous = self.files.insert(id, bytes).unwrap_or(0);
        self.used = self.used.saturating_sub(previous).saturating_add(bytes);
    }
    pub fn release(&mut self, id: u64) {
        self.used = self
            .used
            .saturating_sub(self.files.remove(&id).unwrap_or(0));
    }
}

#[cfg(unix)]
pub fn available_space(path: &Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path =
        std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the path is NUL terminated and stats points to writable storage.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful statvfs initialized the complete structure.
    let stats = unsafe { stats.assume_init() };
    let bytes = u128::from(stats.f_bavail) * u128::from(stats.f_frsize);
    Ok(u64::try_from(bytes).unwrap_or(u64::MAX))
}
#[cfg(windows)]
pub fn available_space(path: &Path) -> std::io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            available: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
    }
    // GetDiskFreeSpaceExW takes a directory and fails with ERROR_DIRECTORY
    // (267) for a file, while callers pass files too — a stage reserves its
    // growth against its own data file — as statvfs allows on Unix. Ask for
    // the nearest existing directory: the path itself or its closest ancestor.
    let probe = path
        .ancestors()
        .find(|candidate| candidate.is_dir())
        .unwrap_or(path);
    let path: Vec<u16> = probe.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0;
    // SAFETY: the path is NUL terminated, available is writable, and Windows
    // explicitly allows null pointers for the two unused output parameters.
    if unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(available)
}
#[cfg(not(any(unix, windows)))]
pub fn available_space(_path: &Path) -> std::io::Result<u64> {
    Err(std::io::Error::other("Unsupported staging filesystem"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_files_share_one_snapshot_inclusive_budget() {
        let mut quota = StageQuota {
            limit: 100,
            ..Default::default()
        };
        assert!(quota.reserve(1, 60));
        assert!(!quota.reserve(2, 50));
        assert!(quota.reserve(2, 40));
        assert!(!quota.reserve(1, 61));
        quota.release(2);
        assert!(quota.reserve(1, 80));
    }
    #[test]
    fn recovery_retains_existing_bytes_even_above_a_new_limit() {
        let mut quota = StageQuota {
            limit: 10,
            ..Default::default()
        };
        quota.restore(1, 20);
        assert!(!quota.reserve(2, 1));
        quota.release(1);
        assert!(quota.reserve(2, 10));
    }
    #[test]
    fn available_space_accepts_a_file_as_well_as_its_folder() {
        let root = std::env::temp_dir().join(format!(
            "r2-available-space-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("record.data");
        std::fs::write(&file, b"staged").unwrap();
        let from_folder = available_space(&root).unwrap();
        let from_file = available_space(&file).unwrap();
        assert!(from_folder > 0);
        assert!(from_file > 0);
        // Windows walks to the nearest existing folder; statvfs needs the path.
        #[cfg(windows)]
        assert!(available_space(&root.join("not-created-yet.data")).unwrap() > 0);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
