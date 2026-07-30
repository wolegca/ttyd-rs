/// File transfer endpoints: upload, download, and directory listing
use crate::config::FileTransferConfig;
use crate::session::SessionManager;
use axum::{
    Json,
    body::Body,
    extract::{Multipart, Query, State},
    http::{StatusCode, header},
    response::Response,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
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
    pub session_id: Option<String>,
    /// Show hidden files (dotfiles) in listing. Default: false
    #[serde(default)]
    pub show_hidden: bool,
}

/// Query parameters for upload (session_id passed via query string)
#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    pub session_id: Option<String>,
    /// Allow overwriting an existing file. Default: false
    #[serde(default)]
    pub overwrite: bool,
}

/// Resolve the base directory for file operations.
///
/// When `file_transfer.dir` is explicitly configured, use it directly.
/// Otherwise, resolve the CWD of the specified session's PTY child process
/// via `/proc/<pid>/cwd`. If no session_id is given, falls back to the most
/// recently active session. If a session_id IS given but not found, returns
/// an error to prevent cross-session directory access.
/// Falls back to the server process CWD only when no session_id is specified.
async fn resolve_base_dir(
    state: &FileTransferState,
    session_id: Option<&str>,
) -> Result<PathBuf, (StatusCode, String)> {
    // Explicitly configured directory takes priority
    if let Some(ref dir) = state.config.dir {
        return Ok(dir.clone());
    }

    // If a specific session is requested, resolve from that session ONLY
    if let Some(sid) = session_id {
        if sid.is_empty() {
            // Empty session_id treated as "no session specified"
        } else if let Some(session) = state.session_manager.get_session(sid).await {
            let pty = session.pty_session();
            let pty_guard = pty.lock().await;
            let pid = pty_guard.child_pid().as_raw();
            drop(pty_guard);

            let cwd_link = PathBuf::from(format!("/proc/{}/cwd", pid));
            match tokio::fs::read_link(&cwd_link).await {
                Ok(cwd) => {
                    debug!(
                        "Resolved file transfer base dir from session {} (pid {}): {}",
                        sid,
                        pid,
                        cwd.display()
                    );
                    return Ok(cwd);
                }
                Err(e) => {
                    debug!(
                        "Failed to read /proc/{}/cwd for session {}: {}",
                        pid, sid, e
                    );
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Cannot resolve working directory for session: {}", e),
                    ));
                }
            }
        } else {
            // Session not found: reject to prevent cross-session access
            return Err((StatusCode::NOT_FOUND, format!("Session not found: {}", sid)));
        }
    }

    // Fallback (only when no session_id specified): most recently active session
    let sessions = state.session_manager.list_sessions().await;
    if !sessions.is_empty() {
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
            let cwd_link = PathBuf::from(format!("/proc/{}/cwd", pid));
            match tokio::fs::read_link(&cwd_link).await {
                Ok(cwd) => {
                    debug!(
                        "Resolved file transfer base dir from most recent pid {}: {}",
                        pid,
                        cwd.display()
                    );
                    return Ok(cwd);
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

    // Final fallback: server process working directory
    Ok(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp")))
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
    Query(query): Query<UploadQuery>,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, (StatusCode, Json<FileErrorResponse>)> {
    let base = resolve_base_dir(&state, query.session_id.as_deref())
        .await
        .map_err(|(status, msg)| (status, Json(FileErrorResponse { error: msg })))?;
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

        // Overwrite protection: reject if file exists and overwrite not requested
        if !query.overwrite && target.exists() {
            return Err((
                StatusCode::CONFLICT,
                Json(FileErrorResponse {
                    error: format!(
                        "File already exists: {}. Use overwrite=true to replace.",
                        sanitized
                    ),
                }),
            ));
        }

        // Stream field data to file with incremental size checking
        let mut file = tokio::fs::File::create(&target).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(FileErrorResponse {
                    error: format!("Cannot create file: {}", e),
                }),
            )
        })?;

        let mut total_size: usize = 0;
        let mut field = field;
        while let Some(chunk) = field.chunk().await.map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(FileErrorResponse {
                    error: format!("Failed to read upload data: {}", e),
                }),
            )
        })? {
            total_size += chunk.len();
            if total_size > max_size {
                // Abort: close and remove partial file
                drop(file);
                let _ = tokio::fs::remove_file(&target).await;
                return Err((
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(FileErrorResponse {
                        error: format!("File too large: exceeds max {} bytes", max_size),
                    }),
                ));
            }
            file.write_all(&chunk).await.map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(FileErrorResponse {
                        error: format!("Failed to write file: {}", e),
                    }),
                )
            })?;
        }

        file.flush().await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(FileErrorResponse {
                    error: format!("Failed to flush file: {}", e),
                }),
            )
        })?;

        uploaded_filename = sanitized;
        uploaded_size = total_size;
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

/// GET /api/files/download?path=... — download a file (streaming)
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

    let base = resolve_base_dir(&state, query.session_id.as_deref())
        .await
        .map_err(|(status, msg)| (status, Json(FileErrorResponse { error: msg })))?;
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

    // Open file for streaming
    let file = tokio::fs::File::open(&target).await.map_err(|e| {
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

    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".to_string());

    // Sanitize filename for Content-Disposition: remove control chars and quotes
    let safe_name: String = file_name
        .chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\')
        .collect();
    let safe_name = if safe_name.is_empty() {
        "download".to_string()
    } else {
        safe_name
    };

    let mime = mime_guess::from_path(&target).first_or_octet_stream();
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CONTENT_LENGTH, metadata.len())
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", safe_name),
        )
        .body(body)
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

/// Core directory listing logic, reusable by both HTTP and WebSocket handlers.
///
/// Resolves the base directory for the given session, then lists entries.
/// Returns `(resolved_path_display, entries)` on success.
pub async fn list_directory(
    state: &FileTransferState,
    session_id: Option<&str>,
    relative: &str,
    show_hidden: bool,
) -> Result<(String, Vec<FileEntry>), (StatusCode, String)> {
    let base = resolve_base_dir(state, session_id).await?;

    let target = safe_resolve(&base, relative)?;

    if !target.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("Directory not found: {}", relative),
        ));
    }

    if !target.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Path is not a directory".to_string(),
        ));
    }

    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(&target).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Cannot read directory: {}", e),
        )
    })?;

    while let Some(entry) = read_dir.next_entry().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Error reading entry: {}", e),
        )
    })? {
        let name = entry.file_name().to_string_lossy().to_string();

        if !show_hidden && name.starts_with('.') {
            continue;
        }

        let metadata = entry.metadata().await.ok();
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

    Ok((relative.to_string(), entries))
}

/// GET /api/files/list?path=... — list directory contents
pub async fn list_files(
    State(state): State<FileTransferState>,
    Query(query): Query<PathQuery>,
) -> Result<Json<ListResponse>, (StatusCode, Json<FileErrorResponse>)> {
    let relative = query.path.as_deref().unwrap_or(".");
    let (path, entries) = list_directory(
        &state,
        query.session_id.as_deref(),
        relative,
        query.show_hidden,
    )
    .await
    .map_err(|(status, msg)| (status, Json(FileErrorResponse { error: msg })))?;

    Ok(Json(ListResponse { path, entries }))
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
        assert_eq!(
            resolve_base_dir(&state, None).await.unwrap(),
            PathBuf::from("/tmp/custom")
        );
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
        assert_eq!(resolve_base_dir(&state, None).await.unwrap(), expected);
    }

    #[tokio::test]
    async fn test_base_dir_invalid_session_returns_error() {
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
        // Non-existent session_id should return 404, NOT fallback
        let result = resolve_base_dir(&state, Some("nonexistent-session")).await;
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);
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
        let resolved = resolve_base_dir(&state, Some("test-pwd")).await.unwrap();
        assert!(resolved.exists());
        assert!(resolved.is_dir());
    }

    #[tokio::test]
    async fn test_list_files_nonexistent_dir() {
        let state = test_state(PathBuf::from("/nonexistent-dir-xyz"));
        let query = PathQuery {
            path: None,
            session_id: None,
            show_hidden: false,
        };
        let result = list_files(State(state), Query(query)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_files_valid_dir() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-list");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("hello.txt"), "world").unwrap();

        let state = test_state(dir.clone());
        let query = PathQuery {
            path: None,
            session_id: None,
            show_hidden: false,
        };
        let result = list_files(State(state), Query(query)).await;
        assert!(result.is_ok());
        let Json(resp) = result.unwrap();
        assert!(resp.entries.iter().any(|e| e.name == "hello.txt"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_list_files_hides_dotfiles_by_default() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-hidden");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("visible.txt"), "yes").unwrap();
        fs::write(dir.join(".secret"), "hidden").unwrap();

        let state = test_state(dir.clone());

        // Default: hidden files filtered
        let query = PathQuery {
            path: None,
            session_id: None,
            show_hidden: false,
        };
        let result = list_files(State(state.clone()), Query(query))
            .await
            .unwrap();
        let Json(resp) = result;
        assert!(resp.entries.iter().any(|e| e.name == "visible.txt"));
        assert!(!resp.entries.iter().any(|e| e.name == ".secret"));

        // show_hidden=true: all files shown
        let query = PathQuery {
            path: None,
            session_id: None,
            show_hidden: true,
        };
        let result = list_files(State(state), Query(query)).await.unwrap();
        let Json(resp) = result;
        assert!(resp.entries.iter().any(|e| e.name == ".secret"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_download_file_not_found() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-dl");
        let _ = fs::create_dir_all(&dir);

        let state = test_state(dir.clone());
        let query = PathQuery {
            path: Some("nonexistent.txt".to_string()),
            session_id: None,
            show_hidden: false,
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
            session_id: None,
            show_hidden: false,
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
            session_id: None,
            show_hidden: false,
        };
        let result = download_file(State(state), Query(query)).await;
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);

        let _ = fs::remove_dir_all(&dir);
    }
}
