use crate::auth::{AuthToken, BrowserToken};
use crate::client::Body;
use crate::common::ApiOutcome;
use crate::error::Error;
use crate::utils::constants::DEFAULT_X_GOOG_AUTHUSER;
use crate::{Client, Result};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;

/// Allowed upload file types - check by trying to upload something outside this
/// list on YTM.
const ALLOWED_UPLOAD_EXTENSIONS: &[&str] = &["mp3", "m4a", "wma", "flac", "ogg"];

const MAX_UPLOAD_FILESIZE_MB: u64 = 300;

pub(crate) fn validate_upload_path(file_path: &Path) -> Result<(tokio::fs::File, u64)> {
    let upload_fileext = file_path
        .extension()
        .and_then(OsStr::to_str)
        .ok_or_else(|| {
            Error::invalid_upload_filename(
                file_path.to_string_lossy().into(),
                "Filename contains invalid chars".into(),
            )
        })?;
    if !ALLOWED_UPLOAD_EXTENSIONS.contains(&upload_fileext) {
        return Err(Error::invalid_upload_filename(
            file_path.to_string_lossy().into(),
            format!("Fileext not in allowed list. Allowed values: {ALLOWED_UPLOAD_EXTENSIONS:?}"),
        ));
    }
    let max_bytes = MAX_UPLOAD_FILESIZE_MB * (1024 * 1024);
    let metadata = std::fs::metadata(file_path)
        .map_err(|e| Error::web(format!("Failed to read file metadata: {e}")))?;
    let upload_filesize_bytes = metadata.len();
    if upload_filesize_bytes > max_bytes {
        return Err(Error::web(format!(
            "Unable to upload song greater than {} MB, size is {} MB",
            MAX_UPLOAD_FILESIZE_MB,
            upload_filesize_bytes / (1024 * 1024)
        )));
    }
    let file = std::fs::File::open(file_path)
        .map(tokio::fs::File::from)
        .map_err(|e| Error::web(format!("Failed to open file: {e}")))?;
    Ok((file, upload_filesize_bytes))
}

/// Upload a song to your YouTube Music Library.
pub async fn upload_song(
    file_path: impl AsRef<Path>,
    token: &BrowserToken,
    client: &Client,
) -> Result<ApiOutcome> {
    let file_path = file_path.as_ref();
    let (song_file, upload_filesize_bytes) = validate_upload_path(file_path)?;

    // Headers to get upload url
    let additional_headers: [(&str, Cow<str>); 4] = [
        (
            "Content-Type",
            "application/x-www-form-urlencoded;charset=utf-8".into(),
        ),
        ("X-Goog-Upload-Command", "start".into()),
        (
            "X-Goog-Upload-Header-Content-Length",
            upload_filesize_bytes.to_string().into(),
        ),
        ("X-Goog-Upload-Protocol", "resumable".into()),
    ];
    // Deduplicate with token's headers.
    let mut combined_headers = token
        .headers()?
        .into_iter()
        .chain(additional_headers)
        .collect::<HashMap<_, _>>();
    let upload_url = client
        .post_query(
            "https://upload.youtube.com/upload/usermusic/http",
            combined_headers
                .iter()
                .map(|(k, v)| (*k, v.as_ref().into())),
            Body::FromString(format!(
                "filename={}",
                file_path
                    .file_name()
                    .ok_or_else(|| {
                        Error::invalid_upload_filename(
                            file_path.to_string_lossy().into(),
                            "Filename contains invalid chars".into(),
                        )
                    })?
                    .to_string_lossy()
            )),
            &[("authuser", DEFAULT_X_GOOG_AUTHUSER)],
        )
        .await?
        .headers
        .into_iter()
        .find(|(k, _)| k == "x-goog-upload-url")
        .ok_or_else(Error::missing_upload_url)?
        .1;
    // Additional headers required to upload.
    combined_headers.extend([
        ("X-Goog-Upload-Command", "upload, finalize".into()),
        ("X-Goog-Upload-Offset", "0".into()),
    ]);
    if client
        .post_query(upload_url, combined_headers, Body::FromFile(song_file), &())
        .await?
        .status_code
        == 200
    {
        Ok(ApiOutcome::Success)
    } else {
        Ok(ApiOutcome::Failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    #[test]
    fn test_validate_rejects_disallowed_extension() {
        let path = tmp_path("song.exe");
        fs::write(&path, b"not a real song").unwrap();
        let result = validate_upload_path(&path);
        fs::remove_file(&path).unwrap();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Fileext"),
            "Expected extension error, got: {err}"
        );
    }

    #[test]
    fn test_validate_rejects_oversized_file() {
        let path = tmp_path("huge.mp3");
        // 301 MB file (above 300 MB limit)
        let size = 301 * 1024 * 1024;
        let file = fs::File::create(&path).unwrap();
        file.set_len(size).unwrap();
        let result = validate_upload_path(&path);
        fs::remove_file(&path).unwrap();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Unable to upload"),
            "Expected size error, got: {err}"
        );
    }

    #[test]
    fn test_validate_accepts_valid_file() {
        let path = tmp_path("valid_song.mp3");
        fs::write(&path, b"fake audio content").unwrap();
        let result = validate_upload_path(&path);
        fs::remove_file(&path).unwrap();
        assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
    }

    #[test]
    fn test_validate_rejects_nonexistent_file() {
        let path = tmp_path("nonexistent.mp3");
        let result = validate_upload_path(&path);
        assert!(result.is_err());
    }
}
