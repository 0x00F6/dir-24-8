//! Atomic file publication: temporary file in the destination directory,
//! optional sync, rename.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::error::SnapshotError;
use super::format::IO_BUF;
use super::options::FileSync;

/// Removes the temporary file unless it was published.
struct Temporary {
    path: PathBuf,
    published: bool,
}

impl Drop for Temporary {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Creates `.<name>.<pid>.<n>.tmp` next to the destination (`create_new`,
/// so an existing file is never reused).
fn create_temporary(dir: &Path, name: &std::ffi::OsStr) -> io::Result<(Temporary, File)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    loop {
        let mut tmp = OsString::from(".");
        tmp.push(name);
        tmp.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let path = dir.join(tmp);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                return Ok((
                    Temporary {
                        path,
                        published: false,
                    },
                    file,
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(unix)]
fn sync_directory(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Writes `path` atomically: `write` streams into a buffered temporary
/// file, which is flushed, given the permissions of the file it replaces,
/// synced according to `sync`, and renamed over `path`. On error the
/// temporary file is removed and `path` is untouched.
pub(crate) fn write_atomic(
    path: &Path,
    sync: FileSync,
    write: impl FnOnce(&mut BufWriter<File>) -> Result<(), SnapshotError>,
) -> Result<(), SnapshotError> {
    let io = SnapshotError::Io;
    let name = path.file_name().ok_or_else(|| {
        io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "snapshot path has no file name",
        ))
    })?;
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let (mut temporary, file) = create_temporary(dir, name).map_err(io)?;
    let mut writer = BufWriter::with_capacity(IO_BUF, file);
    write(&mut writer)?;
    let file = writer.into_inner().map_err(|e| io(e.into_error()))?;
    if let Ok(previous) = fs::metadata(path)
        && previous.is_file()
    {
        file.set_permissions(previous.permissions()).map_err(io)?;
    }
    if sync != FileSync::None {
        file.sync_all().map_err(io)?;
    }
    drop(file);
    fs::rename(&temporary.path, path).map_err(io)?;
    temporary.published = true;
    if sync == FileSync::FileAndDirectory {
        sync_directory(dir).map_err(io)?;
    }
    Ok(())
}
