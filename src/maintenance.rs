//! Read-only log verification and versioned, checksummed full-history backups.

use super::*;
use std::path::PathBuf;

const BACKUP_MAGIC: &[u8; 16] = b"CEDROBAK\0\x01\0\0\0\0\0\0";
const BACKUP_HEADER_BYTES: u64 = 32;
const COPY_BUFFER_BYTES: usize = 65_536;

/// Counts reconstructed from every complete transaction in the durable log.
/// Verification replays documents in memory in addition to the open database's
/// existing state. Encoded content limits exclude allocator and map overhead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrityReport {
    pub log_bytes: u64,
    pub transactions: u64,
    pub records: u64,
    pub content_bytes: u64,
}

struct VerifiedLog {
    report: IntegrityReport,
    checksum: u32,
}

impl Database {
    /// Validate the full log without repairing, truncating, or rewriting it.
    /// Rejects an active staged writer. The handle mutex is held during replay.
    pub fn verify(&self) -> Result<IntegrityReport> {
        let mut inner = self.lock()?;
        if inner.writer_active {
            return Err(Error::WriterBusy);
        }
        let log_bytes = inner.file.metadata()?.len();
        inner.file.seek(SeekFrom::Start(0))?;
        let verified = verify_log(&mut inner.file, log_bytes, self.options)?;
        if inner.file.metadata()?.len() != log_bytes {
            return Err(Error::Corruption(
                "log size changed during verification".into(),
            ));
        }
        Ok(verified.report)
    }

    /// Create a new backup FILE, preserving all transactions and change cursors.
    /// The parent directory must exist, and an existing path is never replaced.
    /// CRC32 detects accidental corruption; the container is not authenticated.
    /// Copies use a 64 KiB buffer; validation also replays live documents in RAM.
    /// The database mutex remains held through verification, copy, and sync.
    pub fn backup(&self, path: impl AsRef<Path>) -> Result<IntegrityReport> {
        let path = path.as_ref();
        let mut inner = self.lock()?;
        if inner.writer_active {
            return Err(Error::WriterBusy);
        }
        let log_bytes = inner.file.metadata()?.len();
        inner.file.seek(SeekFrom::Start(0))?;
        let verified = verify_log(&mut inner.file, log_bytes, self.options)?;
        publish_atomically(path, |backup| {
            backup.write_all(BACKUP_MAGIC)?;
            backup.write_all(&log_bytes.to_le_bytes())?;
            backup.write_all(&verified.checksum.to_le_bytes())?;
            backup.write_all(&0u32.to_le_bytes())?;
            inner.file.seek(SeekFrom::Start(0))?;
            let checksum = copy_checked(&mut inner.file, backup, log_bytes)?;
            if checksum != verified.checksum || inner.file.metadata()?.len() != log_bytes {
                return Err(Error::Corruption(
                    "log changed while creating backup".into(),
                ));
            }
            Ok(())
        })?;
        Ok(verified.report)
    }

    /// Restore a backup into a new directory using default content limits.
    /// The destination directory must not exist, even if it would be empty.
    pub fn restore_backup(
        backup: impl AsRef<Path>,
        destination: impl AsRef<Path>,
    ) -> Result<IntegrityReport> {
        Self::restore_backup_with_options(backup, destination, Options::default())
    }

    /// Validate a full backup before creating the destination directory.
    /// Its parent must exist. Creation never overwrites existing directories,
    /// files, or symlinks. The newly created log stays exclusively locked until
    /// its contents and directory entries have been synced. On failure, cleanup
    /// attempts to remove only this operation's own log and empty directory.
    pub fn restore_backup_with_options(
        backup: impl AsRef<Path>,
        destination: impl AsRef<Path>,
        options: Options,
    ) -> Result<IntegrityReport> {
        options.validate()?;
        let mut source = File::open(backup)?;
        // Hold a cooperating-reader lock across validation and copy. External
        // programs must not edit a backup during restoration.
        FileExt::try_lock_shared(&source).map_err(map_lock_error)?;
        let (log_bytes, checksum) = read_backup_header(&mut source)?;
        if checksum_reader(&mut source, log_bytes)? != checksum {
            return Err(Error::Corruption("backup payload checksum mismatch".into()));
        }
        source.seek(SeekFrom::Start(BACKUP_HEADER_BYTES))?;
        let verified = verify_log(&mut source, log_bytes, options)?;
        if verified.checksum != checksum {
            return Err(Error::Corruption(
                "backup changed during verification".into(),
            ));
        }
        let destination = destination.as_ref();
        // Refuse an existing destination up front (also dangling symlinks).
        if fs::symlink_metadata(destination).is_ok() {
            return Err(Error::Io(io::Error::from(io::ErrorKind::AlreadyExists)));
        }
        // Build in a private sibling directory and publish it by rename only
        // when complete, so the final name never holds a partial restore.
        let staging = staging_path(destination)?;
        fs::create_dir(&staging)?;
        let log_path = staging.join("cedro.log");
        let outcome = (|| {
            let mut target = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&log_path)?;
            lock_exclusive(&target)?;
            source.seek(SeekFrom::Start(BACKUP_HEADER_BYTES))?;
            let copied_checksum = copy_checked(&mut source, &mut target, log_bytes)?;
            if copied_checksum != checksum
                || source.metadata()?.len() != BACKUP_HEADER_BYTES + log_bytes
            {
                return Err(Error::Corruption(
                    "backup changed during restoration".into(),
                ));
            }
            target.sync_all()?;
            sync_directory(&staging)?;
            drop(target);
            fs::rename(&staging, destination)?;
            sync_directory(parent_directory(destination))?;
            Ok(verified.report)
        })();
        if outcome.is_err() && fs::symlink_metadata(&staging).is_ok() {
            let _ = fs::remove_file(&log_path);
            let _ = fs::remove_dir(&staging);
        }
        outcome
    }
}

/// Private sibling name for building a directory before publishing it.
fn staging_path(destination: &Path) -> Result<PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = destination
        .file_name()
        .ok_or_else(|| Error::Io(io::Error::from(io::ErrorKind::InvalidInput)))?;
    let mut temp = std::ffi::OsString::from(".");
    temp.push(name);
    temp.push(format!(
        ".{}.{}.restoring",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    Ok(parent_directory(destination).join(temp))
}

fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn map_lock_error(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::WouldBlock
        || error
            .raw_os_error()
            .is_some_and(|code| Some(code) == fs2::lock_contended_error().raw_os_error())
    {
        Error::DatabaseLocked
    } else {
        Error::Io(error)
    }
}

/// Write a file to a private temp name beside `path`, sync it, then publish it
/// without replacing anything (hard link fails if `path` exists), and sync the
/// directory. The final name never holds a partial file; the temp is removed.
fn publish_atomically(path: &Path, write: impl FnOnce(&mut File) -> Result<()>) -> Result<()> {
    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let directory = parent_directory(path);
    let name = path
        .file_name()
        .ok_or_else(|| Error::Io(std::io::Error::from(std::io::ErrorKind::InvalidInput)))?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let temp = directory.join(temp_name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let outcome: Result<()> = (|| {
        lock_exclusive(&file)?;
        write(&mut file)?;
        file.sync_all()?;
        fs::hard_link(&temp, path)?;
        Ok(())
    })();
    drop(file);
    // The temp name is exclusively ours; the final name is never removed.
    let removed = fs::remove_file(&temp);
    outcome?;
    removed?;
    sync_directory(directory)
}

fn lock_exclusive(file: &File) -> Result<()> {
    FileExt::try_lock_exclusive(file).map_err(map_lock_error)
}

fn read_checked(reader: &mut impl Read, bytes: &mut [u8], context: &str) -> Result<()> {
    reader.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            Error::Corruption(format!("incomplete {context}"))
        } else {
            Error::Io(error)
        }
    })
}

fn read_backup_header(source: &mut File) -> Result<(u64, u32)> {
    let mut header = [0u8; BACKUP_HEADER_BYTES as usize];
    read_checked(source, &mut header, "backup header")?;
    if &header[..16] != BACKUP_MAGIC || header[28..32] != [0; 4] {
        return Err(Error::Corruption(
            "unsupported or invalid backup header".into(),
        ));
    }
    let log_bytes = u64::from_le_bytes(header[16..24].try_into().unwrap());
    let expected_bytes = BACKUP_HEADER_BYTES
        .checked_add(log_bytes)
        .ok_or_else(|| Error::Corruption("backup length overflow".into()))?;
    if log_bytes < HEADER.len() as u64 || source.metadata()?.len() != expected_bytes {
        return Err(Error::Corruption(
            "backup length does not match complete expected payload".into(),
        ));
    }
    let checksum = u32::from_le_bytes(header[24..28].try_into().unwrap());
    Ok((log_bytes, checksum))
}

fn checksum_reader(reader: &mut impl Read, bytes: u64) -> Result<u32> {
    copy_checked(reader, &mut io::sink(), bytes)
}

fn copy_checked(reader: &mut impl Read, writer: &mut impl Write, mut bytes: u64) -> Result<u32> {
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    let mut checksum = crc32fast::Hasher::new();
    while bytes != 0 {
        let count = bytes.min(buffer.len() as u64) as usize;
        read_checked(reader, &mut buffer[..count], "backup payload")?;
        writer.write_all(&buffer[..count])?;
        checksum.update(&buffer[..count]);
        bytes -= count as u64;
    }
    Ok(checksum.finalize())
}

fn verify_log(reader: &mut impl Read, log_bytes: u64, options: Options) -> Result<VerifiedLog> {
    if log_bytes < HEADER.len() as u64 {
        return Err(Error::Corruption("incomplete database header".into()));
    }
    let mut header = [0u8; HEADER.len()];
    read_checked(reader, &mut header, "database header")?;
    if &header != HEADER {
        return Err(Error::Corruption(
            "unsupported or invalid database header".into(),
        ));
    }
    let mut checksum = crc32fast::Hasher::new();
    checksum.update(&header);
    let mut state = State::default();
    let mut offset = HEADER.len() as u64;
    let mut transactions = 0u64;
    while offset < log_bytes {
        if log_bytes - offset < FRAME_HEADER_BYTES as u64 {
            return Err(Error::Corruption(format!(
                "incomplete frame header at byte {offset}"
            )));
        }
        let mut frame_header = [0u8; FRAME_HEADER_BYTES];
        read_checked(reader, &mut frame_header, "transaction header")?;
        let length = u32::from_le_bytes(frame_header[..4].try_into().unwrap());
        let inverted = u32::from_le_bytes(frame_header[4..8].try_into().unwrap());
        let expected_checksum = u32::from_le_bytes(frame_header[8..12].try_into().unwrap());
        if inverted != !length
            || (length as usize) < EMPTY_BATCH_BYTES
            || length as usize > options.max_frame_bytes
        {
            return Err(Error::Corruption(format!(
                "invalid frame length at byte {offset}"
            )));
        }
        let frame_bytes = FRAME_HEADER_BYTES as u64 + u64::from(length);
        if log_bytes - offset < frame_bytes {
            return Err(Error::Corruption(format!(
                "incomplete transaction at byte {offset}"
            )));
        }
        let mut payload = vec![0; length as usize];
        read_checked(reader, &mut payload, "transaction payload")?;
        if crc32fast::hash(&payload) != expected_checksum {
            return Err(Error::Corruption(format!(
                "checksum mismatch at byte {offset}"
            )));
        }
        let batch: Batch = serde_json::from_slice(&payload).map_err(|error| {
            Error::Corruption(format!("invalid transaction at byte {offset}: {error}"))
        })?;
        let (changes, state_bytes) =
            prepare_changes_owned(&state, batch.ops, options).map_err(|error| {
                Error::Corruption(format!("invalid transaction at byte {offset}: {error}"))
            })?;
        apply_changes(&mut state, changes, state_bytes);
        checksum.update(&frame_header);
        checksum.update(&payload);
        transactions += 1;
        offset += frame_bytes;
    }
    Ok(VerifiedLog {
        report: IntegrityReport {
            log_bytes,
            transactions,
            records: state
                .collections
                .values()
                .map(|documents| documents.len() as u64)
                .sum(),
            content_bytes: state.content_bytes as u64,
        },
        checksum: checksum.finalize(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "cedrodb-maintenance-private-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn verify_does_not_repair_a_torn_live_log_or_publish_its_backup() {
        let directory = TestDirectory::new();
        let database = Database::open(directory.0.join("database")).unwrap();
        database.put("docs", "committed", json!({"n":1})).unwrap();
        let valid_bytes = database.stats().unwrap().log_bytes;
        {
            // Inject damage through the owning descriptor: Windows correctly
            // prevents an unrelated descriptor from writing through this lock.
            let mut inner = database.inner.lock().unwrap();
            inner.file.seek(SeekFrom::End(0)).unwrap();
            inner.file.write_all(&[1, 2, 3]).unwrap();
            inner.file.sync_all().unwrap();
        }
        assert!(matches!(database.verify(), Err(Error::Corruption(_))));
        let backup = directory.0.join("must-not-exist.cedrobak");
        assert!(matches!(
            database.backup(&backup),
            Err(Error::Corruption(_))
        ));
        assert!(!backup.exists());
        assert_eq!(database.stats().unwrap().log_bytes, valid_bytes + 3);
        assert!(database.get("docs", "committed").unwrap().is_some());
        {
            let mut inner = database.inner.lock().unwrap();
            inner.file.seek(SeekFrom::Start(valid_bytes)).unwrap();
            let mut tail = [0; 3];
            inner.file.read_exact(&mut tail).unwrap();
            assert_eq!(tail, [1, 2, 3]);
        }
    }

    #[test]
    fn poisoned_handle_rejects_verification_and_backup_without_filesystem_writes() {
        let directory = TestDirectory::new();
        let database = Database::open(directory.0.join("database")).unwrap();
        database.inner.lock().unwrap().poisoned = true;
        let backup = directory.0.join("must-not-exist.cedrobak");
        assert!(matches!(database.verify(), Err(Error::Poisoned)));
        assert!(matches!(database.backup(&backup), Err(Error::Poisoned)));
        assert!(!backup.exists());
    }

    #[test]
    fn failed_publication_leaves_no_final_or_temp_file() {
        let directory = TestDirectory::new();
        let target = directory.0.join("out.cedrobak");
        let result = publish_atomically(&target, |file| {
            file.write_all(b"partial")?;
            Err(Error::Corruption("injected".into()))
        });
        assert!(matches!(result, Err(Error::Corruption(_))));
        assert!(!target.exists());
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
    }

    #[test]
    fn publication_refuses_existing_destination_and_keeps_it() {
        let directory = TestDirectory::new();
        let target = directory.0.join("out.cedrobak");
        fs::write(&target, b"original").unwrap();
        let result = publish_atomically(&target, |file| Ok(file.write_all(b"new")?));
        assert!(result.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
        let database = Database::open(directory.0.join("db")).unwrap();
        assert!(database.backup(&target).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original");
    }

    #[test]
    fn successful_backup_leaves_only_the_final_file() {
        let directory = TestDirectory::new();
        let database = Database::open(directory.0.join("db")).unwrap();
        database.put("docs", "a", json!({"n":1})).unwrap();
        let target = directory.0.join("ok.cedrobak");
        database.backup(&target).unwrap();
        let names: Vec<_> = fs::read_dir(&directory.0)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(target.exists());
    }
}
