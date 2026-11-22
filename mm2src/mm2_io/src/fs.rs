use async_std::fs as async_fs;
use common::log::{error, LogOnError};
use derive_more::Display;
use futures::AsyncWriteExt;
use gstuff::{try_s, ERR, ERRL};
use mm2_err_handle::prelude::*;
use rand::random;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{self as json, Error as JsonError};
use std::ffi::OsStr;
use std::fs::{self, DirEntry};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

pub type FsJsonResult<T> = Result<T, MmError<FsJsonError>>;
pub type IoResult<T> = Result<T, MmError<io::Error>>;

#[derive(Debug, Display)]
pub enum FsJsonError {
    IoReading(io::Error),
    IoWriting(io::Error),
    Serializing(JsonError),
    Deserializing(JsonError),
}

pub fn check_dir_operations(dir_path: &Path) -> Result<(), io::Error> {
    let r: [u8; 32] = random();
    let mut check: Vec<u8> = Vec::with_capacity(r.len());
    let fname = dir_path.join("checkval");
    let mut fp = fs::File::create(&fname)?;
    fp.write_all(&r)?;
    drop(fp);
    let mut fp = fs::File::open(&fname)?;
    fp.read_to_end(&mut check)?;
    if check.len() != r.len() {
        return Err(io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("Expected same data length when reading file: {:?}", fname),
        ));
    }
    if check != r {
        return Err(io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Expected the same {:?} data: {:?} != {:?}", fname, r, check),
        ));
    }
    Ok(())
}

/// Invokes `OS_ensure_directory`,
/// then prints an error and returns `false` if the directory is not writable.
pub fn ensure_dir_is_writable(dir_path: &Path) -> bool {
    if dir_path.exists() && !dir_path.is_dir() {
        error!("The {} is not a directory", dir_path.display());
        return false;
    } else if let Err(e) = std::fs::create_dir_all(dir_path) {
        error!("Could not create dir {}, error {}", dir_path.display(), e);
        return false;
    }
    check_dir_operations(dir_path).error_log_passthrough().is_ok()
}

pub fn ensure_file_is_writable(file_path: &Path) -> Result<(), String> {
    if fs::File::open(file_path).is_err() {
        // try to create file if opening fails
        if let Err(e) = fs::OpenOptions::new().write(true).create_new(true).open(file_path) {
            return ERR!("{} when trying to create the file {}", e, file_path.display());
        }
    } else {
        // try to open file in write append mode
        if let Err(e) = fs::OpenOptions::new().append(true).open(file_path) {
            return ERR!(
                "{} when trying to open the file {} in write mode",
                e,
                file_path.display()
            );
        }
    }
    Ok(())
}

pub fn slurp(path: &dyn AsRef<Path>) -> Result<Vec<u8>, String> { Ok(gstuff::slurp(path)) }

pub fn safe_slurp(path: &dyn AsRef<Path>) -> Result<Vec<u8>, String> {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(ref err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return ERR!("Can't open {:?}: {}", path.as_ref(), err),
    };
    let mut buf = Vec::new();
    try_s!(file.read_to_end(&mut buf));
    Ok(buf)
}

pub fn remove_file(path: &dyn AsRef<Path>) -> Result<(), String> {
    try_s!(fs::remove_file(path));
    Ok(())
}

pub async fn remove_file_async<P: AsRef<Path>>(path: P) -> IoResult<()> {
    Ok(async_fs::remove_file(path.as_ref()).await?)
}

pub fn write(path: &dyn AsRef<Path>, contents: &dyn AsRef<[u8]>) -> Result<(), String> {
    try_s!(fs::write(path, contents));
    Ok(())
}

/// Read a folder asynchronously and return a list of files.
pub async fn read_dir_async<P: AsRef<Path>>(dir: P) -> IoResult<Vec<PathBuf>> {
    use futures::StreamExt;

    let mut result = Vec::new();
    let mut entries = async_fs::read_dir(dir.as_ref()).await?;

    while let Some(entry) = entries.next().await {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                error!("Error '{}' reading from dir {}", e, dir.as_ref().display());
                continue;
            },
        };
        result.push(entry.path().into());
    }
    Ok(result)
}

/// Read a folder and return a list of files with their last-modified ms timestamps.
pub fn read_dir(dir: &dyn AsRef<Path>) -> Result<Vec<(u64, PathBuf)>, String> {
    let entries = try_s!(dir.as_ref().read_dir())
        .filter_map(|dir_entry| {
            let entry = match dir_entry {
                Ok(ent) => ent,
                Err(e) => {
                    error!("Error '{}' reading from dir {}", e, dir.as_ref().display());
                    return None;
                },
            };

            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    error!("Error '{}' getting file {} meta", e, entry.path().display());
                    return None;
                },
            };

            let m_time = match metadata.modified() {
                Ok(time) => time,
                Err(e) => {
                    error!("Error '{}' getting file {} m_time", e, entry.path().display());
                    return None;
                },
            };

            let lm = m_time.duration_since(UNIX_EPOCH).expect("!duration_since").as_millis();
            assert!(lm < u64::MAX as u128);
            let lm = lm as u64;

            let path = entry.path();
            if path.extension() == Some(OsStr::new("json")) {
                Some((lm, path))
            } else {
                None
            }
        })
        .collect();

    Ok(entries)
}

pub async fn read_json<T>(path: &Path) -> FsJsonResult<Option<T>>
where
    T: DeserializeOwned,
{
    let content = match async_fs::read(path).await {
        Ok(content) => content,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return MmError::err(FsJsonError::IoReading(e)),
    };
    json::from_slice(&content).map_to_mm(FsJsonError::Deserializing)
}

/// Read the `dir_path` entries trying to deserialize each as the `T` type.
/// Please note that files that couldn't be deserialized are skipped.
pub async fn read_dir_json<T>(dir_path: &Path) -> FsJsonResult<Vec<T>>
where
    T: DeserializeOwned,
{
    let json_ext = Some(OsStr::new("json"));
    let entries: Vec<_> = read_dir_async(dir_path)
        .await
        .mm_err(FsJsonError::IoReading)?
        .into_iter()
        .filter(|path| path.extension() == json_ext)
        .collect();
    let type_name = std::any::type_name::<T>();

    let mut result = Vec::new();
    for file_path in entries {
        match read_json(&file_path).await {
            Ok(Some(t)) => result.push(t),
            Ok(None) => {
                error!(
                    "Expected '{}' type at the file {}, found 'None'",
                    type_name,
                    file_path.display()
                );
                continue;
            },
            Err(e) => {
                error!(
                    "Error reading '{}' from the file {}: {}",
                    type_name,
                    file_path.display(),
                    e
                );
                continue;
            },
        };
    }
    Ok(result)
}

/// Process-wide counter that makes every temp file name of [`write_json`] unique within the process.
static TMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp path next to `path`: `<file name>.<process id>.<counter>.tmp`. Two writers of the same target never
/// share one.
fn unique_tmp_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_else(|| OsStr::new("file"));
    let mut tmp_name = name.to_os_string();
    tmp_name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        TMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(tmp_name)
}

/// Serialize `t` as JSON into the file at `path`.
///
/// With `use_tmp_file = true` the content is written to a unique temp file in the same directory, flushed and
/// synced to disk, renamed over `path`, and then (on Unix) the directory is synced. The target therefore holds
/// either its old or its new complete content, never a truncated one, also after a crash or power loss. On an
/// error the temp file is removed (best effort) and the original error is returned. Left-over `*.tmp` files
/// (e.g. after a crash) are ignored by [`read_dir_json`], [`read_dir`] and [`json_dir_entries`], which read only
/// `*.json`.
///
/// With `use_tmp_file = false` the target is truncated and rewritten in place, without a sync.
pub async fn write_json<T>(t: &T, path: &Path, use_tmp_file: bool) -> FsJsonResult<()>
where
    T: Serialize,
{
    let content = json::to_vec(t).map_to_mm(FsJsonError::Serializing)?;

    let res: io::Result<()> = if use_tmp_file {
        write_atomically(&content, path).await
    } else {
        let fs_fut = async {
            let mut file = async_fs::File::create(path).await?;
            file.write_all(&content).await?;
            file.flush().await?;
            Ok(())
        };
        fs_fut.await
    };
    res.map_to_mm(FsJsonError::IoWriting)
}

async fn write_atomically(content: &[u8], path: &Path) -> io::Result<()> {
    let path_tmp = unique_tmp_path(path);
    let fs_fut = async {
        let mut file = async_fs::File::create(&path_tmp).await?;
        file.write_all(content).await?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);

        async_fs::rename(&path_tmp, path).await?;

        // Make the rename itself durable. Windows cannot open a directory as a file, so the directory sync is
        // skipped there (the rename is a `MoveFileEx` replace).
        #[cfg(unix)]
        {
            let dir = match path.parent() {
                Some(dir) if !dir.as_os_str().is_empty() => dir,
                _ => Path::new("."),
            };
            async_fs::File::open(dir).await?.sync_all().await?;
        }
        Ok(())
    };

    let res: io::Result<()> = fs_fut.await;
    if res.is_err() {
        // Best effort: after a successful rename the temp path no longer exists and this is a no-op.
        let _ = async_fs::remove_file(&path_tmp).await;
    }
    res
}

pub fn json_dir_entries(path: &dyn AsRef<Path>) -> Result<Vec<DirEntry>, String> {
    Ok(try_s!(path.as_ref().read_dir())
        .filter_map(|dir_entry| {
            let entry = match dir_entry {
                Ok(ent) => ent,
                Err(e) => {
                    error!("Error '{}' reading from dir {}", e, path.as_ref().display());
                    return None;
                },
            };

            if entry.path().extension() == Some(OsStr::new("json")) {
                Some(entry)
            } else {
                None
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use serde_json::json;

    fn new_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mm2-io-{}-{}-{}", tag, std::process::id(), random::<u64>()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn write_json_with_tmp_file_replaces_content_and_leaves_no_tmp() {
        let dir = new_dir("replace");
        let path = dir.join("a.json");
        block_on(write_json(&json!({"v": 1}), &path, true)).unwrap();
        block_on(write_json(&json!({"v": 2}), &path, true)).unwrap();

        let read: Option<serde_json::Value> = block_on(read_json(&path)).unwrap();
        assert_eq!(read, Some(json!({"v": 2})));
        assert_eq!(file_names(&dir), vec!["a.json".to_owned()]);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The failure is injected by making the target a non-empty directory: the temp file is written and synced,
    /// then the rename onto the directory fails.
    #[test]
    fn failed_rename_keeps_old_content_and_removes_tmp() {
        let dir = new_dir("failed-rename");
        let path = dir.join("a.json");
        // A non-empty directory cannot be replaced by a file. It stands for the "existing target".
        fs::create_dir_all(path.join("inner")).unwrap();
        fs::write(path.join("inner").join("old"), b"old").unwrap();

        let res = block_on(write_json(&json!({"v": 1}), &path, true));
        assert!(res.is_err());

        assert_eq!(fs::read(path.join("inner").join("old")).unwrap(), b"old");
        assert_eq!(
            file_names(&dir),
            vec!["a.json".to_owned()],
            "the temp file must be removed"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_dir_json_ignores_left_over_tmp_files() {
        let dir = new_dir("leftover");
        block_on(write_json(&json!({"v": 1}), &dir.join("a.json"), true)).unwrap();
        fs::write(dir.join("x.json.tmp"), b"{\"v\": 2}").unwrap();
        fs::write(dir.join("y.json.123.0.tmp"), b"{\"v\": 3}").unwrap();

        let read: Vec<serde_json::Value> = block_on(read_dir_json(&dir)).unwrap();
        assert_eq!(read, vec![json!({"v": 1})]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn concurrent_writes_end_with_one_complete_record() {
        let dir = new_dir("concurrent");
        let path = dir.join("a.json");
        let big = |c: char| json!({ "v": c.to_string().repeat(100_000) });

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                let value = big((b'a' + i as u8) as char);
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        block_on(write_json(&value, &path, true)).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let read: serde_json::Value = block_on(read_json(&path)).unwrap().expect("the record exists");
        let v = read["v"].as_str().unwrap();
        assert_eq!(v.len(), 100_000);
        assert!(
            v.chars().all(|c| c == v.chars().next().unwrap()),
            "the record is one complete write"
        );
        assert_eq!(file_names(&dir), vec!["a.json".to_owned()]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
