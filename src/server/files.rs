/// File transfer endpoints: upload, download, and directory listing
use crate::config::FileTransferConfig;
use crate::session::SessionManager;
use axum::{
    Json,
    extract::{Multipart, Query, State},
    http::{StatusCode, header},
    response::Response,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tracing::debug;

/// Shared state for file transfer handlers
#[derive(Clone)]
pub struct FileTransferState {
    pub config: Arc<FileTransferConfig>,
    pub session_manager: Arc<SessionManager>,
}

/// Error response for file operations
#[derive(Debug, Serialize)]
pub struct FileErrorResponse {
    pub error: String,
}

/// A single entry in a directory listing
#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub modified: Option<String>,
}

/// Response for directory listing
#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub path: String,
    pub entries: Vec<FileEntry>,
}

/// Response for successful upload
#[derive(Debug, Serialize)]
pub struct UploadResponse {
    pub filename: String,
    pub size: usize,
}

/// Query parameters for download/list
#[derive(Debug, Deserialize)]
pub struct PathQuery {
    pub path: Option<String>,
}

/// Resolve the base directory for file operations.
///
/// When `file_transfer.dir` is explicitly configured, use it directly.
/// Otherwise, dynamically resolve the CWD of the most recently active
/// session's PTY child process via `/proc/<pid>/cwd`. This makes file
/// operations follow the terminal's `$PWD` as the user `cd`s around.
/// Falls back to the server process CWD if no active session exists.
async fn resolve_base_dir(state: &FileTransferState) -> PathBuf {
    // Explicitly configured directory takes priority
    if let Some(ref dir) = state.config.dir {
        return dir.clone();
    }

    // Try to resolve from the most recently active session's PTY process
    let sessions = state.session_manager.list_sessions().await;
    if !sessions.is_empty() {
        // Find the session with the most recent activity
        let mut most_recent: Option<(std::time::Instant, i32)> = None;
        for session in &sessions {
            let activity = session.last_activity().await;
            let pty = session.pty_session();
            let pty_guard = pty.lock().await;
            let pid = pty_guard.child_pid().as_raw();
            drop(pty_guard);

            match most_recent {
                Some((ref t, _)) if activity > *t => {
                    most_recent = Some((activity, pid));
                }
                None => {
                    most_recent = Some((activity, pid));
                }
                _ => {}
            }
        }

        if let Some((_, pid)) = most_recent {
            // Read /proc/<pid>/cwd symlink to get the shell's current directory
            let cwd_link = PathBuf::from(format!("/proc/{}/cwd", pid));
            match tokio::fs::read_link(&cwd_link).await {
                Ok(cwd) => {
                    debug!(
                        "Resolved file transfer base dir from pid {}: {}",
                        pid,
                        cwd.display()
                    );
                    return cwd;
                }
                Err(e) => {
                    debug!(
                        "Failed to read /proc/{}/cwd: {}, falling back to process CWD",
                        pid, e
                    );
                }
            }
        }
    }

    // Fallback: server process working directory
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
}

/// Validate that a resolved path does not escape the base directory.
/// Returns the canonicalized path on success.
fn safe_resolve(base: &Path, relative: &str) -> Result<PathBuf, (StatusCode, String)> {
    // Reject absolute paths and obvious traversal attempts early
    let candidate = base.join(relative);

    // Canonicalize the base to get a stable prefix
    let canonical_base = base.canonicalize().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Base directory not accessible".to_string(),
        )
    })?;

    // For files that don't exist yet (upload), canonicalize the parent
    let canonical_candidate = if candidate.exists() {
        candidate
            .canonicalize()
            .map_err(|_| (StatusCode::FORBIDDEN, "Path resolution failed".to_string()))?
    } else {
        // Canonicalize parent directory, then append filename
        let parent = candidate
            .parent()
            .ok_or_else(|| (StatusCode::FORBIDDEN, "Invalid path".to_string()))?;
        let canonical_parent = parent.canonicalize().map_err(|_| {
            (
                StatusCode::FORBIDDEN,
                "Parent directory not accessible".to_string(),
            )
        })?;
        let file_name = candidate
            .file_name()
            .ok_or_else(|| (StatusCode::FORBIDDEN, "Invalid filename".to_string()))?;
        canonical_parent.join(file_name)
    };

    // Ensure the resolved path is within the base directory
    if !canonical_candidate.starts_with(&canonical_base) {
        return Err((StatusCode::FORBIDDEN, "Path traversal detected".to_string()));
    }

    Ok(canonical_candidate)
}

/// POST /api/files/upload — upload a file via multipart/form-data
pub async fn upload_file(
    State(state): State<FileTransferState>,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, (StatusCode, Json<FileErrorResponse>)> {
    let base = resolve_base_dir(&state).await;
    let max_size = state.config.max_upload_size;

    // Ensure base directory exists
    if !base.exists() {
        tokio::fs::create_dir_all(&base).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(FileErrorResponse {
                    error: format!("Cannot create base directory: {}", e),
                }),
            )
        })?;
    }

    let mut uploaded_filename = String::new();
    let mut uploaded_size: usize = 0;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(FileErrorResponse {
                error: format!("Invalid multipart data: {}", e),
            }),
        )
    })? {
        // Get filename from the field
        let filename = match field.file_name() {
            Some(name) => name.to_string(),
            None => continue, // Skip non-file fields
        };

        if filename.is_empty() {
            continue;
        }

        // Sanitize filename: strip any path components
        let sanitized = Path::new(&filename)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        if sanitized.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(FileErrorResponse {
                    error: "Invalid filename".to_string(),
                }),
            ));
        }

        // Validate target path
        let target = safe_resolve(&base, &sanitized)
            .map_err(|(status, msg)| (status, Json(FileErrorResponse { error: msg })))?;

        // Read field data with size limit
        let data = field.bytes().await.map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(FileErrorResponse {
                    error: format!("Failed to read upload data: {}", e),
                }),
            )
        })?;

        if data.len() > max_size {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(FileErrorResponse {
                    error: format!(
                        "File too large: {} bytes (max: {} bytes)",
                        data.len(),
                        max_size
                    ),
                }),
            ));
        }

        // Write file
        tokio::fs::write(&target, &data).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(FileErrorResponse {
                    error: format!("Failed to write file: {}", e),
                }),
            )
        })?;

        uploaded_filename = sanitized;
        uploaded_size = data.len();
    }

    if uploaded_filename.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(FileErrorResponse {
                error: "No file provided in upload".to_string(),
            }),
        ));
    }

    Ok(Json(UploadResponse {
        filename: uploaded_filename,
        size: uploaded_size,
    }))
}

/// GET /api/files/download?path=... — download a file
pub async fn download_file(
    State(state): State<FileTransferState>,
    Query(query): Query<PathQuery>,
) -> Result<Response, (StatusCode, Json<FileErrorResponse>)> {
    let relative = query.path.as_deref().unwrap_or("");

    if relative.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(FileErrorResponse {
                error: "Missing 'path' query parameter".to_string(),
            }),
        ));
    }

    let base = resolve_base_dir(&state).await;
    let target = safe_resolve(&base, relative)
        .map_err(|(status, msg)| (status, Json(FileErrorResponse { error: msg })))?;

    if !target.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(FileErrorResponse {
                error: format!("File not found: {}", relative),
            }),
        ));
    }

    if target.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(FileErrorResponse {
                error: "Path is a directory, not a file".to_string(),
            }),
        ));
    }

    // Read file content
    let mut file = tokio::fs::File::open(&target).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(FileErrorResponse {
                error: format!("Cannot open file: {}", e),
            }),
        )
    })?;

    let metadata = file.metadata().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(FileErrorResponse {
                error: format!("Cannot read file metadata: {}", e),
            }),
        )
    })?;

    let mut buf = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut buf).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(FileErrorResponse {
                error: format!("Cannot read file: {}", e),
            }),
        )
    })?;

    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".to_string());

    let mime = mime_guess::from_path(&target).first_or_octet_stream();

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CONTENT_LENGTH, buf.len())
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", file_name),
        )
        .body(axum::body::Body::from(buf))
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(FileErrorResponse {
                    error: "Failed to build response".to_string(),
                }),
            )
        })?;

    Ok(response)
}

/// GET /api/files/list?path=... — list directory contents
pub async fn list_files(
    State(state): State<FileTransferState>,
    Query(query): Query<PathQuery>,
) -> Result<Json<ListResponse>, (StatusCode, Json<FileErrorResponse>)> {
    let relative = query.path.as_deref().unwrap_or(".");
    let base = resolve_base_dir(&state).await;

    let target = safe_resolve(&base, relative)
        .map_err(|(status, msg)| (status, Json(FileErrorResponse { error: msg })))?;

    if !target.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(FileErrorResponse {
                error: format!("Directory not found: {}", relative),
            }),
        ));
    }

    if !target.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(FileErrorResponse {
                error: "Path is not a directory".to_string(),
            }),
        ));
    }

    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(&target).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(FileErrorResponse {
                error: format!("Cannot read directory: {}", e),
            }),
        )
    })?;

    while let Some(entry) = read_dir.next_entry().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(FileErrorResponse {
                error: format!("Error reading directory entry: {}", e),
            }),
        )
    })? {
        let metadata = entry.metadata().await.ok();
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = metadata.as_ref().is_some_and(|m| m.is_dir());
        let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified = metadata.as_ref().and_then(|m| m.modified().ok()).map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                .to_string()
        });

        entries.push(FileEntry {
            name,
            size,
            is_dir,
            modified,
        });
    }

    // Sort: directories first, then by name
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));

    Ok(Json(ListResponse {
        path: relative.to_string(),
        entries,
    }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::session::{SessionManager, SessionMode};
    use std::fs;
    use std::time::Duration;

    fn test_state(dir: PathBuf) -> FileTransferState {
        FileTransferState {
            config: Arc::new(FileTransferConfig {
                enabled: true,
                dir: Some(dir),
                max_upload_size: 1024,
            }),
            session_manager: Arc::new(SessionManager::new(
                Duration::from_secs(3600),
                SessionMode::Isolated,
            )),
        }
    }

    #[test]
    fn test_safe_resolve_valid_path() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-resolve");
        let _ = fs::create_dir_all(&dir);

        let result = safe_resolve(&dir, "test.txt");
        assert!(result.is_ok());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_safe_resolve_traversal_blocked() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-traversal");
        let _ = fs::create_dir_all(&dir);

        let result = safe_resolve(&dir, "../../etc/passwd");
        assert!(result.is_err());
        let (status, msg) = result.unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(msg.contains("traversal"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_safe_resolve_absolute_path_blocked() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-absolute");
        let _ = fs::create_dir_all(&dir);

        // Absolute path joined with base still resolves outside
        let result = safe_resolve(&dir, "/etc/passwd");
        // This should either be forbidden or resolve within base
        if let Ok(resolved) = &result {
            let canonical_base = dir.canonicalize().unwrap();
            assert!(resolved.starts_with(&canonical_base));
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_safe_resolve_subdirectory() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-subdir");
        let subdir = dir.join("sub");
        let _ = fs::create_dir_all(&subdir);

        let result = safe_resolve(&dir, "sub/file.txt");
        assert!(result.is_ok());

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_base_dir_uses_config() {
        let state = test_state(PathBuf::from("/tmp/custom"));
        assert_eq!(resolve_base_dir(&state).await, PathBuf::from("/tmp/custom"));
    }

    #[tokio::test]
    async fn test_base_dir_defaults_to_cwd_when_no_session() {
        let state = FileTransferState {
            config: Arc::new(FileTransferConfig {
                enabled: true,
                dir: None,
                max_upload_size: 1024,
            }),
            session_manager: Arc::new(SessionManager::new(
                Duration::from_secs(3600),
                SessionMode::Isolated,
            )),
        };
        // No active sessions, should fall back to process CWD
        let expected = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
        assert_eq!(resolve_base_dir(&state).await, expected);
    }

    #[tokio::test]
    async fn test_base_dir_follows_session_pwd() {
        // Create a session with a long-running process
        let sm = Arc::new(SessionManager::new(
            Duration::from_secs(3600),
            SessionMode::Isolated,
        ));
        sm.create_session(
            "test-pwd".to_string(),
            &["sleep".to_string(), "30".to_string()],
            None,
            80,
            24,
            None,
        )
        .await
        .unwrap();

        let state = FileTransferState {
            config: Arc::new(FileTransferConfig {
                enabled: true,
                dir: None,
                max_upload_size: 1024,
            }),
            session_manager: sm,
        };

        // The resolved dir should be a valid existing directory (the shell's CWD)
        let resolved = resolve_base_dir(&state).await;
        assert!(resolved.exists());
        assert!(resolved.is_dir());
    }

    #[tokio::test]
    async fn test_list_files_nonexistent_dir() {
        let state = test_state(PathBuf::from("/nonexistent-dir-xyz"));
        let query = PathQuery { path: None };
        let result = list_files(State(state), Query(query)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_files_valid_dir() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-list");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("hello.txt"), "world").unwrap();

        let state = test_state(dir.clone());
        let query = PathQuery { path: None };
        let result = list_files(State(state), Query(query)).await;
        assert!(result.is_ok());
        let Json(resp) = result.unwrap();
        assert!(resp.entries.iter().any(|e| e.name == "hello.txt"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_download_file_not_found() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-dl");
        let _ = fs::create_dir_all(&dir);

        let state = test_state(dir.clone());
        let query = PathQuery {
            path: Some("nonexistent.txt".to_string()),
        };
        let result = download_file(State(state), Query(query)).await;
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_download_file_success() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-dl2");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("data.bin"), b"binary content").unwrap();

        let state = test_state(dir.clone());
        let query = PathQuery {
            path: Some("data.bin".to_string()),
        };
        let result = download_file(State(state), Query(query)).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_download_traversal_blocked() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-dl3");
        let _ = fs::create_dir_all(&dir);

        let state = test_state(dir.clone());
        let query = PathQuery {
            path: Some("../../etc/passwd".to_string()),
        };
        let result = download_file(State(state), Query(query)).await;
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);

        let _ = fs::remove_dir_all(&dir);
    }
}
