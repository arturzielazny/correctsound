//! The app's own files (sessions, settings, calibration files), read and written as with
//! `std::fs`: on the disk in the installed program. A web page cannot write to the disk, so
//! there they are kept in memory for as long as the page is open.

#[cfg(not(target_family = "wasm"))]
pub use std::fs::{create_dir_all, read, read_dir, read_to_string, write};

#[cfg(target_family = "wasm")]
pub use memory::{create_dir_all, read, read_dir, read_to_string, write};

#[cfg(target_family = "wasm")]
mod memory {
    use std::collections::BTreeMap;
    use std::io::{Error, ErrorKind, Result};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static FILES: Mutex<BTreeMap<PathBuf, Vec<u8>>> = Mutex::new(BTreeMap::new());

    fn files() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, Vec<u8>>> {
        FILES.lock().expect("file store lock poisoned")
    }

    fn missing(path: &Path) -> Error {
        Error::new(
            ErrorKind::NotFound,
            format!("{} does not exist", path.display()),
        )
    }

    /// Folders exist as soon as a file is written into them.
    pub fn create_dir_all(_path: impl AsRef<Path>) -> Result<()> {
        Ok(())
    }

    pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
        files().insert(path.as_ref().to_path_buf(), contents.as_ref().to_vec());
        Ok(())
    }

    pub fn read(path: impl AsRef<Path>) -> Result<Vec<u8>> {
        let path = path.as_ref();
        files().get(path).cloned().ok_or_else(|| missing(path))
    }

    pub fn read_to_string(path: impl AsRef<Path>) -> Result<String> {
        String::from_utf8(read(path)?).map_err(|e| Error::new(ErrorKind::InvalidData, e))
    }

    /// A folder's entry.
    #[derive(Debug)]
    pub struct DirEntry(PathBuf);

    impl DirEntry {
        #[must_use]
        pub fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    /// The files and folders directly in `dir`.
    pub fn read_dir(dir: impl AsRef<Path>) -> Result<std::vec::IntoIter<Result<DirEntry>>> {
        let dir = dir.as_ref();
        let mut found: Vec<PathBuf> = files()
            .keys()
            .filter_map(|p| p.strip_prefix(dir).ok())
            .filter_map(|rest| rest.components().next())
            .map(|first| dir.join(first))
            .collect();
        found.dedup();
        if found.is_empty() {
            return Err(missing(dir));
        }
        let entries: Vec<Result<DirEntry>> = found.into_iter().map(|p| Ok(DirEntry(p))).collect();
        Ok(entries.into_iter())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn written_files_read_back() {
        let dir = std::env::temp_dir().join(format!("correctsound-files-{}", std::process::id()));
        super::create_dir_all(dir.join("a")).expect("folder");
        super::write(dir.join("a/one.txt"), "one").expect("written");
        assert_eq!(
            super::read_to_string(dir.join("a/one.txt")).expect("read"),
            "one"
        );
        let entries: Vec<_> = super::read_dir(&dir)
            .expect("listed")
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(entries, vec![dir.join("a")]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
