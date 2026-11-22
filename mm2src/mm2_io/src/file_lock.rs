use common::log::warn;
use derive_more::Display;
use gstuff::{now_float, now_ms};
use mm2_err_handle::prelude::*;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

pub type FileLockResult<T> = std::result::Result<T, MmError<FileLockError>>;

#[derive(Debug, Display)]
pub enum FileLockError {
    #[display(fmt = "Error reading timestamp from {:?}: {}", path, error)]
    ErrorReadingTimestamp { path: PathBuf, error: String },
    #[display(fmt = "Error writing timestamp to {:?}: {}", path, error)]
    ErrorWritingTimestamp { path: PathBuf, error: String },
    #[display(fmt = "Error creating {:?}: {}", path, error)]
    ErrorCreatingLockFile { path: PathBuf, error: String },
    /// This runner no longer holds the lock (CRD ch.44 R44.9A.13 item 3): the file is gone or unreadable as text,
    /// holds a value this lock did not write, or refreshes kept failing past the lock's time limit.
    #[display(fmt = "Lock {:?} is lost: {}", path, reason)]
    LockLost { path: PathBuf, reason: String },
}

/// What this lock last wrote (CRD ch.44 R44.9A.13 item 3). The fencing data is kept in memory only: the content of
/// the lock file stays the bare Unix time of R44.9A.7, so a reference build still respects it.
struct LockState {
    /// The value this lock last wrote to the file.
    value: u64,
    /// When that write completed (monotonic clock).
    written_at: Instant,
    /// Set once a refresh found the lock lost; a lost lock writes and removes nothing.
    lost: bool,
}

pub struct FileLock<T: AsRef<Path>> {
    /// Filesystem path of the lock file.
    lock_path: T,
    /// The time in seconds after which an outdated lock file can be removed.
    ttl_sec: f64,
    state: Mutex<LockState>,
}

/// Process-wide counter that makes every temp file name of this module unique within the process.
static TMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp path next to `path`: `<lock file name>.<process id>.<counter>.tmp` (extension `tmp`, R44.9A.13 item 1).
fn unique_tmp_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_else(|| std::ffi::OsStr::new("lock"));
    let mut tmp_name = name.to_os_string();
    tmp_name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        TMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(tmp_name)
}

/// Rename `from` over `to`. On Windows a rename can be refused transiently (`PermissionDenied`, sharing or lock
/// violation) while another process briefly holds a file, so it is retried a few times with short sleeps; this code
/// is synchronous, so the sleeps block. This mirrors the retry of `fs::rename_replace`. Unix does not retry.
fn rename_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        for delay_ms in [20u64, 40, 80, 160, 320] {
            match std::fs::rename(from, to) {
                Err(e)
                    if e.kind() == io::ErrorKind::PermissionDenied
                        || matches!(e.raw_os_error(), Some(32) | Some(33)) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                },
                res => return res,
            }
        }
    }
    std::fs::rename(from, to)
}

/// Replaces the content of `path` with `timestamp` (ASCII decimal digits only, R44.9A.7) through a temp file in the
/// same directory and a rename, so a reader never sees empty or partial content (closes gap 1 of the R44.9A.9
/// finding). The temp file is removed if any step fails.
fn write_timestamp(path: &Path, timestamp: u64) -> io::Result<()> {
    let tmp = unique_tmp_path(path);
    let res = std::fs::write(&tmp, timestamp.to_string()).and_then(|_| rename_replace(&tmp, path));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// R44.9A.8: the content is the reference form only if it is non-empty ASCII decimal digits and nothing else.
/// Anything else that is text (empty, signed, fractional, trailing white space, JSON, ...) counts as free.
fn parse_timestamp(content: &str) -> Option<u64> {
    if content.is_empty() || !content.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    content.parse().ok()
}

/// Attempts to read timestamp recorded to a file
fn read_timestamp(path: &dyn AsRef<Path>) -> FileLockResult<Option<u64>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(parse_timestamp(&content)),
        Err(e) => MmError::err(FileLockError::ErrorReadingTimestamp {
            path: path.as_ref().to_path_buf(),
            error: e.to_string(),
        }),
    }
}

/// Writes the current time to `path` and reads the file back once. Returns the written value if the file still
/// holds it, `None` if another writer replaced it in between. This narrows gap 2 of the R44.9A.9 finding between
/// RELOADED runners only; it does not close it against a concurrent reference build, whose take-over is a plain
/// write that can land after this read (R44.9A.13 item 5).
fn write_and_verify(path: &Path, between: impl FnOnce()) -> FileLockResult<Option<u64>> {
    let value = now_ms() / 1000;
    write_timestamp(path, value).map_to_mm(|error| FileLockError::ErrorWritingTimestamp {
        path: path.to_path_buf(),
        error: error.to_string(),
    })?;
    between();
    let content = std::fs::read_to_string(path).map_to_mm(|error| FileLockError::ErrorReadingTimestamp {
        path: path.to_path_buf(),
        error: error.to_string(),
    })?;
    Ok((content == value.to_string()).then_some(value))
}

impl<T: AsRef<Path>> FileLock<T> {
    pub fn lock(lock_path: T, ttl_sec: f64) -> FileLockResult<Option<FileLock<T>>> {
        Self::lock_with(lock_path, ttl_sec, || ())
    }

    /// [`lock`](Self::lock) with a hook that runs between the write that takes the lock and the read-back.
    fn lock_with(lock_path: T, ttl_sec: f64, between: impl FnOnce()) -> FileLockResult<Option<FileLock<T>>> {
        // The handle is closed at once: a file still open here would make the rename in `take` fail on Windows.
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path.as_ref())
            .map(drop);
        match created {
            Ok(()) => {
                let path = lock_path.as_ref().to_path_buf();
                let res = Self::take(lock_path, ttl_sec, between);
                if res.is_err() {
                    // The file was created empty by this call and no complete value was written: do not leave
                    // it behind (an empty file would read as a free lock).
                    let _ = std::fs::remove_file(path);
                }
                res
            },
            Err(ref ie) if ie.kind() == io::ErrorKind::AlreadyExists => {
                // See if the existing lock is old enough to be discarded.
                match read_timestamp(&lock_path)? {
                    Some(lm) if now_float() - (lm as f64) <= ttl_sec => Ok(None),
                    // Stale, or content that is not the reference form (R44.9A.8): take it.
                    _ => Self::take(lock_path, ttl_sec, between),
                }
            },
            Err(ie) => MmError::err(FileLockError::ErrorCreatingLockFile {
                path: lock_path.as_ref().to_path_buf(),
                error: ie.to_string(),
            }),
        }
    }

    fn take(lock_path: T, ttl_sec: f64, between: impl FnOnce()) -> FileLockResult<Option<FileLock<T>>> {
        match write_and_verify(lock_path.as_ref(), between)? {
            Some(value) => Ok(Some(FileLock {
                lock_path,
                ttl_sec,
                state: Mutex::new(LockState {
                    value,
                    written_at: Instant::now(),
                    lost: false,
                }),
            })),
            // Someone else wrote the file after this runner: not acquired, and the file is not this runner's.
            None => Ok(None),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, LockState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Refreshes the lock. Fails with `LockLost` (and writes nothing) if this runner no longer holds it
    /// (R44.9A.13 item 3): the file is missing or not text, or its content is not the value this lock last wrote
    /// (a take-over always writes a different value). A refresh that comes late, with the file still holding this
    /// lock's own value, is not lost: nobody took the lock (for example after a host suspend), so it is refreshed
    /// and a warning is logged. The time limit applies only to failures: if a refresh fails with an I/O error and
    /// this lock's last successful write is older than the limit, the lock is treated as lost, because another
    /// runner may have found it stale meanwhile. A single I/O error within the limit is returned as it is and the
    /// caller retries at its next tick.
    pub fn touch(&self) -> FileLockResult<()> {
        let path = self.lock_path.as_ref();
        let mut state = self.state();
        let lost = |state: &mut LockState, reason: String| {
            state.lost = true;
            MmError::err(FileLockError::LockLost {
                path: path.to_path_buf(),
                reason,
            })
        };
        if state.lost {
            return lost(&mut state, "already marked lost".to_owned());
        }
        let age = state.written_at.elapsed().as_secs_f64();
        let too_old = age > self.ttl_sec;
        let too_old_reason = |age: f64| {
            format!(
                "refresh failed and the last successful write was {:.0} s ago, over the {} s limit",
                age, self.ttl_sec
            )
        };
        match std::fs::read_to_string(path) {
            Ok(content) if content == state.value.to_string() => (),
            Ok(content) => {
                return lost(
                    &mut state,
                    format!("content {:?} is not the value this lock wrote", content),
                )
            },
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::InvalidData) => {
                return lost(&mut state, format!("file is missing or not text: {}", e));
            },
            Err(e) => {
                if too_old {
                    return lost(&mut state, too_old_reason(age));
                }
                return MmError::err(FileLockError::ErrorReadingTimestamp {
                    path: path.to_path_buf(),
                    error: e.to_string(),
                });
            },
        }
        // The value check above detects a take-over; a late refresh that still finds this lock's own value means
        // nobody took the lock.
        let value = now_ms() / 1000;
        if let Err(error) = write_timestamp(path, value) {
            if too_old {
                return lost(&mut state, too_old_reason(age));
            }
            return MmError::err(FileLockError::ErrorWritingTimestamp {
                path: path.to_path_buf(),
                error: error.to_string(),
            });
        }
        if too_old {
            warn!(
                "Lock {:?} was refreshed {:.0} s after its last write, over the {} s limit; nobody took it",
                path, age, self.ttl_sec
            );
        }
        state.value = value;
        state.written_at = Instant::now();
        Ok(())
    }
}

impl<T: AsRef<Path>> Drop for FileLock<T> {
    /// Removes the file only if this lock is not lost and the file still holds the value this lock last wrote
    /// (closes gap 3 of the R44.9A.9 finding); otherwise the file belongs to another runner and stays.
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.lost {
            return;
        }
        if std::fs::read_to_string(self.lock_path.as_ref()).is_ok_and(|c| c == state.value.to_string()) {
            let _ = std::fs::remove_file(&self.lock_path);
        }
    }
}

#[cfg(test)]
mod file_lock_tests {
    use super::*;
    use std::{thread::sleep, time::Duration};

    #[test]
    fn test_file_lock_should_create_file_and_record_timestamp_and_then_delete_on_drop() {
        let now = now_ms() / 1000;
        let path = Path::new("test1.lock");
        let lock = FileLock::lock(&path, 1000.).unwrap().unwrap();
        assert!(path.exists());
        let timestamp = read_timestamp(&path).unwrap().unwrap();
        assert!(timestamp >= now);
        drop(lock);
        assert!(!path.exists());
    }

    #[test]
    fn test_file_lock_should_return_none_if_lock_acquired() {
        let path = Path::new("test2.lock");
        let _lock = FileLock::lock(&path, 1000.).unwrap().unwrap();
        let new_lock = FileLock::lock(&path, 1000.).unwrap();
        assert!(new_lock.is_none());
    }

    #[test]
    fn test_file_lock_should_acquire_if_ttl_expired_and_update_timestamp() {
        let path = Path::new("test3.lock");
        let _lock = FileLock::lock(&path, 1.).unwrap().unwrap();
        sleep(Duration::from_secs(2));
        let old_timestamp = read_timestamp(&path).unwrap();
        let _new_lock = FileLock::lock(&path, 1.).unwrap().unwrap();
        let new_timestamp = read_timestamp(&path).unwrap();
        assert!(new_timestamp > old_timestamp);
    }

    #[test]
    fn test_file_lock_should_acquire_if_file_is_empty() {
        let now = now_ms() / 1000;
        let path = Path::new("test4.lock");
        std::fs::write(path, []).unwrap();
        let _new_lock = FileLock::lock(&path, 1.).unwrap().unwrap();
        let timestamp = read_timestamp(&path).unwrap().unwrap();
        assert!(timestamp >= now);
    }

    #[test]
    fn test_file_lock_should_acquire_if_file_does_not_contain_parsable_timestamp() {
        let now = now_ms() / 1000;
        let path = Path::new("test5.lock");
        std::fs::write(path, [12, 13]).unwrap();
        let _new_lock = FileLock::lock(&path, 1.).unwrap().unwrap();
        let timestamp = read_timestamp(&path).unwrap().unwrap();
        assert!(timestamp >= now);
    }

    fn new_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("file-lock-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn assert_digits_now(path: &Path) {
        let bytes = std::fs::read(path).unwrap();
        assert!(
            !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit),
            "content: {:?}",
            bytes
        );
        let value: u64 = String::from_utf8(bytes).unwrap().parse().unwrap();
        let now = now_ms() / 1000;
        assert!(value <= now && now - value <= 5, "value {} now {}", value, now);
    }

    fn dir_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// T44.9A.1: after take and after each refresh the content is ASCII digits only and the current time.
    #[test]
    fn t44_9a_1_content_is_digits_only_after_take_and_refresh() {
        let dir = new_dir("t1");
        let path = dir.join("a.lock");
        let lock = FileLock::lock(&path, 40.).unwrap().unwrap();
        assert_digits_now(&path);
        lock.touch().unwrap();
        assert_digits_now(&path);
        lock.touch().unwrap();
        assert_digits_now(&path);
        // No temp file is left behind by take or refresh.
        assert_eq!(dir_names(&dir), vec!["a.lock".to_owned()]);
        drop(lock);
        assert!(dir_names(&dir).is_empty());
    }

    /// T44.9A.2: a reference-form lock with the current time is not taken; one older than 40 s is.
    #[test]
    fn t44_9a_2_reference_form_lock_is_read_by_the_40_s_rule() {
        let dir = new_dir("t2");
        let path = dir.join("b.lock");
        std::fs::write(&path, (now_ms() / 1000).to_string()).unwrap();
        assert!(FileLock::lock(&path, 40.).unwrap().is_none());
        // A value in the future counts as held.
        std::fs::write(&path, (now_ms() / 1000 + 1000).to_string()).unwrap();
        assert!(FileLock::lock(&path, 40.).unwrap().is_none());
        std::fs::write(&path, (now_ms() / 1000 - 41).to_string()).unwrap();
        let lock = FileLock::lock(&path, 40.).unwrap().expect("a stale lock is taken");
        assert_digits_now(&path);
        drop(lock);
        // Content that is text but not the reference form is free (R44.9A.8, third row): signed, white space.
        for content in ["+5", "123\n", ""] {
            std::fs::write(&path, content).unwrap();
            let lock = FileLock::lock(&path, 40.).unwrap().expect(content);
            assert_digits_now(&path);
            drop(lock);
        }
        // Not valid text is an error (fourth row).
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(FileLock::lock(&path, 40.).is_err());
    }

    #[test]
    fn touch_after_take_over_by_another_writer_is_lock_lost_and_drop_leaves_the_file() {
        let dir = new_dir("takeover");
        let path = dir.join("c.lock");
        let lock = FileLock::lock(&path, 40.).unwrap().unwrap();
        // Another runner found the lock stale and wrote its own value.
        std::fs::write(&path, (now_ms() / 1000 + 100).to_string()).unwrap();
        let err = lock.touch().unwrap_err();
        assert!(matches!(err.into_inner(), FileLockError::LockLost { .. }));
        // Nothing was written by the failed touch, and it stays lost.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            (now_ms() / 1000 + 100).to_string()
        );
        assert!(lock.touch().is_err());
        drop(lock);
        assert!(path.exists(), "a lost lock must not remove the other owner's file");
        assert_eq!(dir_names(&dir), vec!["c.lock".to_owned()]);
    }

    #[test]
    fn touch_with_missing_file_is_lock_lost() {
        let dir = new_dir("missing");
        let path = dir.join("d.lock");
        let lock = FileLock::lock(&path, 40.).unwrap().unwrap();
        std::fs::remove_file(&path).unwrap();
        let err = lock.touch().unwrap_err();
        assert!(matches!(err.into_inner(), FileLockError::LockLost { .. }));
        assert!(!path.exists(), "a lost touch writes nothing");
    }

    /// A late refresh whose file still holds this lock's own value is not lost (nobody took the lock).
    #[test]
    fn late_refresh_with_own_value_succeeds() {
        let dir = new_dir("late");
        let path = dir.join("e.lock");
        let lock = FileLock::lock(&path, 40.).unwrap().unwrap();
        lock.state().written_at = Instant::now() - std::time::Duration::from_secs(41);
        lock.touch().unwrap();
        assert_digits_now(&path);
        assert!(!lock.state().lost);
        assert!(lock.state().written_at.elapsed().as_secs() < 5, "the state was updated");
        lock.touch().unwrap();
        drop(lock);
        assert!(!path.exists(), "the lock is still ours and is removed on drop");
    }

    /// The time limit applies to failed refreshes: the last own write is 41 s back and the refresh fails with an
    /// I/O error (the lock path is a directory, so reading it is neither NotFound nor not-text).
    #[test]
    fn failed_refresh_after_ttl_since_own_last_write_is_lock_lost() {
        let dir = new_dir("ttlfail");
        let path = dir.join("f2.lock");
        let lock = FileLock::lock(&path, 40.).unwrap().unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        // Within the limit a single I/O error is only an error to retry.
        let err = lock.touch().unwrap_err();
        assert!(matches!(err.into_inner(), FileLockError::ErrorReadingTimestamp { .. }));
        assert!(!lock.state().lost);
        lock.state().written_at = Instant::now() - std::time::Duration::from_secs(41);
        let err = lock.touch().unwrap_err();
        assert!(matches!(err.into_inner(), FileLockError::LockLost { .. }));
        assert!(path.is_dir(), "nothing was written or removed");
    }

    #[test]
    fn drop_removes_the_file_only_while_it_holds_this_locks_value() {
        let dir = new_dir("drop");
        let path = dir.join("f.lock");
        let lock = FileLock::lock(&path, 40.).unwrap().unwrap();
        // Replaced by another value, but touch has not run yet (lock not marked lost).
        std::fs::write(&path, "1").unwrap();
        drop(lock);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "1");
    }

    /// The read-back check: another writer replaces the file between the write and the read-back.
    #[test]
    fn take_reports_not_acquired_if_another_writer_replaced_the_file_before_read_back() {
        let dir = new_dir("readback");
        let path = dir.join("g.lock");
        std::fs::write(&path, (now_ms() / 1000 - 100).to_string()).unwrap();
        let other = path.clone();
        let res = FileLock::lock_with(&path, 40., move || std::fs::write(&other, "7").unwrap()).unwrap();
        assert!(res.is_none());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "7",
            "the other writer's file is left alone"
        );
        assert_eq!(dir_names(&dir), vec!["g.lock".to_owned()]);
    }

    #[test]
    fn write_timestamp_leaves_no_tmp_file_when_the_rename_fails() {
        let dir = new_dir("renamefail");
        let target = dir.join("h.lock");
        std::fs::create_dir(&target).unwrap(); // a rename of a file onto a directory fails
        assert!(write_timestamp(&target, 5).is_err());
        assert_eq!(dir_names(&dir), vec!["h.lock".to_owned()]);
    }
}
