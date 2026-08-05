//! Re-usable core functionality.
use anyhow::bail;
use futures::TryStreamExt;
use futures::stream::FuturesUnordered;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::borrow::Borrow;
use std::convert::Infallible;
use std::fmt;
use std::marker::PhantomData;
use std::ops::Add;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::PoisonError;
use tokio::fs::DirEntry;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReadDirStream;
use tracing::warn;

/// Send a message to the specified Tokio mpsc::Sender, and if sending fails,
/// log an error with Tracing.
pub(crate) fn blocking_send_or_error<T, S: Borrow<mpsc::Sender<T>>>(tx: S, msg: T) {
    tx.borrow()
        .blocking_send(msg)
        .unwrap_or_else(|e| warn!("Error {e} received when sending message"));
}

/// Search directory for files matching the pattern {filename}{NUMBER}.{filext}
/// and ext fileext, creating one at {filename}{NUMBER+1}.{filext}.
/// If there are more than max_files with this pattern, delete the
/// oldest surplus ones.
pub(crate) async fn get_limited_sequential_file(
    dir: &Path,
    filename: impl AsRef<str>,
    fileext: impl AsRef<str>,
    max_files: u16,
) -> Result<(fs_err::tokio::File, PathBuf), anyhow::Error> {
    if max_files == 0 {
        bail!("Requested zero file handles")
    }
    let filename = filename.as_ref();
    let fileext = fileext.as_ref();
    let stream = tokio::fs::read_dir(dir).await?;
    #[derive(Debug)]
    struct ValidEntry {
        entry: DirEntry,
        file_number: usize,
    }
    let get_valid_entry = |entry: DirEntry| {
        let entry_file_name = entry.file_name().into_string().ok()?;
        if !entry_file_name.starts_with(filename) || !entry_file_name.ends_with(fileext) {
            return None;
        }
        let file_number = entry_file_name
            .trim_start_matches(filename)
            .trim_end_matches(fileext)
            .trim_end_matches(".")
            .parse::<usize>()
            .ok()?;
        Some(ValidEntry { entry, file_number })
    };
    let mut entries = ReadDirStream::new(stream)
        .filter_map(|try_entry| {
            let entry = match try_entry {
                Ok(entry) => entry,
                Err(e) => return Some(Err(e)),
            };
            get_valid_entry(entry).map(Ok)
        })
        .collect::<Result<Vec<ValidEntry>, _>>()
        .await?;
    entries.sort_by_key(|f| f.file_number);
    let next_number = entries.last().map(|e| e.file_number + 1).unwrap_or(0);
    let next_filename = format!("{filename}{next_number}.{fileext}");
    // If there are max_files files or more, remove the extra files.
    let surplus_files = entries
        .len()
        // Add an additional 1, as we are going to create a file bringing us up to max_files.
        .add(1)
        .saturating_sub(max_files as usize);
    let _deleted_count: usize = entries
        .into_iter()
        .take(surplus_files)
        .map(|entry| fs_err::tokio::remove_file(entry.entry.path()))
        .collect::<FuturesUnordered<_>>()
        .try_collect::<Vec<_>>()
        .await?
        .len();
    let next_filepath = dir.join(next_filename);
    Ok((
        fs_err::tokio::File::create_new(&next_filepath).await?,
        next_filepath,
    ))
}

/// From serde documentation: [<https://serde.rs/string-or-struct.html>]
pub(crate) fn string_or_struct<'de, T, D>(deserializer: D) -> std::result::Result<T, D::Error>
where
    T: Deserialize<'de> + FromStr<Err = Infallible>,
    D: Deserializer<'de>,
{
    // This is a Visitor that forwards string types to T's `FromStr` impl and
    // forwards map types to T's `Deserialize` impl. The `PhantomData` is to
    // keep the compiler from complaining about T being an unused generic type
    // parameter. We need T in order to know the Value type for the Visitor
    // impl.
    struct StringOrStruct<T>(PhantomData<fn() -> T>);
    impl<'de, T> Visitor<'de> for StringOrStruct<T>
    where
        T: Deserialize<'de> + FromStr<Err = Infallible>,
    {
        type Value = T;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("string or map")
        }
        fn visit_str<E>(self, value: &str) -> std::result::Result<T, E>
        where
            E: de::Error,
        {
            Ok(FromStr::from_str(value).unwrap_or_else(|e| match e {}))
        }
        fn visit_map<M>(self, map: M) -> std::result::Result<T, M::Error>
        where
            M: MapAccess<'de>,
        {
            // `MapAccessDeserializer` is a wrapper that turns a `MapAccess`
            // into a `Deserializer`, allowing it to be used as the input to T's
            // `Deserialize` implementation. T then deserializes itself using
            // the entries from the map visitor.
            Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))
        }
    }
    deserializer.deserialize_any(StringOrStruct(PhantomData))
}

/// Extension trait for recovering from poisoned mutexes/RwLocks with a warning.
pub(crate) trait PoisonRecovery {
    type Inner;
    fn unwrap_or_warn(self) -> Self::Inner;
}

impl<T> PoisonRecovery for Result<T, PoisonError<T>> {
    type Inner = T;
    fn unwrap_or_warn(self) -> T {
        match self {
            Ok(v) => v,
            Err(e) => {
                warn!("Lock poisoned: {e}");
                e.into_inner()
            }
        }
    }
}

/// Extract a readable message from a panic payload (the `Box<dyn Any + Send>`
/// captured by `catch_unwind`). Must take the payload as `&Box`, not `&dyn Any`,
/// because `downcast_ref` on a materialized `&(dyn Any + Send)` reference
/// silently fails while auto-deref through the Box works.
pub(crate) fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
        .to_string()
}

/// Get monotonically increasing file handles with prefix filename and ext
/// fileext, but if there are more than max_files with this pattern, delete the
/// lowest one first.
#[cfg(test)]
mod tests {
    use crate::core::get_limited_sequential_file;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;
    use tokio_stream::StreamExt;
    use tokio_stream::wrappers::ReadDirStream;

    #[test]
    fn panic_message_normalizes_panic_payload() {
        use crate::core::panic_message;
        let str_payload: Box<dyn std::any::Any + Send> =
            std::panic::catch_unwind(|| panic!("boom")).unwrap_err();
        assert_eq!(panic_message(&str_payload), "boom");

        let owned_payload: Box<dyn std::any::Any + Send> =
            std::panic::catch_unwind(|| panic!("{}", String::from("boom2"))).unwrap_err();
        assert_eq!(panic_message(&owned_payload), "boom2");

        let opaque: Box<dyn std::any::Any + Send> = Box::new(42u32);
        assert_eq!(panic_message(&opaque), "unknown panic");
    }

    #[tokio::test]
    async fn test_get_limited_sequential_file_has_correct_filename() {
        let tmpdir = TempDir::new().unwrap();
        let _file = get_limited_sequential_file(tmpdir.path(), "test_filename", "txt", 5)
            .await
            .unwrap();
        let filename = fs_err::tokio::read_dir(tmpdir.path())
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .unwrap()
            .file_name()
            .into_string()
            .unwrap();
        assert!(filename.starts_with("test_filename"));
        assert!(filename.ends_with(".txt"));
        let timestamp = filename
            .trim_start_matches("test_filename")
            .trim_end_matches(".txt");
        assert!(timestamp.parse::<usize>().is_ok())
    }
    #[tokio::test]
    async fn test_get_limited_sequential_file_deletes_oldest() {
        let tmpdir = TempDir::new().unwrap();
        let _f1 = get_limited_sequential_file(tmpdir.path(), "test_filename", "txt", 2)
            .await
            .unwrap();
        let f1_name = fs_err::tokio::read_dir(tmpdir.path())
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .unwrap()
            .file_name()
            .into_string()
            .unwrap();
        let _f2 = get_limited_sequential_file(tmpdir.path(), "test_filename", "txt", 2)
            .await
            .unwrap();
        let files_count = std::fs::read_dir(tmpdir.path()).unwrap().count();
        assert_eq!(files_count, 2);
        let _f3 = get_limited_sequential_file(tmpdir.path(), "test_filename", "txt", 2)
            .await
            .unwrap();
        let files_count = std::fs::read_dir(tmpdir.path()).unwrap().count();
        assert_eq!(files_count, 2);
        assert!(
            fs_err::tokio::File::open(tmpdir.path().join(f1_name))
                .await
                .is_err(),
            "_f1 should have been deleted"
        )
    }
    #[tokio::test]
    async fn test_get_limited_sequential_file_doesnt_delete_others() {
        let tmpdir = TempDir::new().unwrap();
        let _f = get_limited_sequential_file(tmpdir.path(), "test_filename", "txt", 1)
            .await
            .unwrap();
        let (Ok(_f1), Ok(_f2)) = tokio::join!(
            fs_err::tokio::File::create_new(tmpdir.path().join("xxx.txt")),
            fs_err::tokio::File::create_new(tmpdir.path().join("test_filename_xxx")),
        ) else {
            panic!("Error creating test files")
        };
        let _f = get_limited_sequential_file(tmpdir.path(), "test_filename", "txt", 1)
            .await
            .unwrap();
        let files_in_dir = ReadDirStream::new(tokio::fs::read_dir(tmpdir.path()).await.unwrap())
            .collect::<Vec<_>>()
            .await;
        assert_eq!(files_in_dir.len(), 3);
        assert!(
            fs_err::tokio::File::open(tmpdir.path().join("test_filename_xxx"))
                .await
                .is_ok(),
            "test_filename_xxx should not have been deleted"
        );
        assert!(
            fs_err::tokio::File::open(tmpdir.path().join("xxx.txt"))
                .await
                .is_ok(),
            "xxx.txt should not have been deleted"
        )
    }
}
