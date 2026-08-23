/// File transfer endpoints: upload, download, and directory listing
use crate::config::FileTransferConfig;
use crate::session::SessionManager;
use axum::{
    Json,
    body::Body,
    extract::{Multipart, Query, State},
    http::{HeaderMap, StatusCode, header},
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

/// RAII guard that removes the upload target file if the upload does not
/// complete successfully. Call `disarm()` on success to keep the file.
///
/// This ensures that no matter which error path is taken (read failure,
/// write failure, size exceeded, connection abort, etc.), the partially
/// written file is cleaned up.
struct UploadFileGuard(PathBuf);

impl UploadFileGuard {
    fn new(path: PathBuf) -> Self {
        Self(path)
    }

    /// Disarm the guard, keeping the file on disk.
    /// Must be called when the upload has completed successfully.
    fn disarm(&mut self) {
        self.0 = PathBuf::new();
    }
}

impl Drop for UploadFileGuard {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            // Best-effort cleanup; the file may have already been removed
            // or the path may be invalid. Ignore errors.
            let _ = std::fs::remove_file(&self.0);
        }
    }
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
/// via `/proc/<pid>/cwd`. A non-empty `session_id` is **required** in that
/// case: falling back to "the most recently active session" would let one
/// client reach into another user's isolated session working directory
/// simply by omitting the parameter, and a silent fallback to the server
/// process CWD would widen the accessible surface unexpectedly.
async fn resolve_base_dir(
    state: &FileTransferState,
    session_id: Option<&str>,
) -> Result<PathBuf, (StatusCode, String)> {
    // Explicitly configured directory takes priority
    if let Some(ref dir) = state.config.dir {
        return Ok(dir.clone());
    }

    // No configured directory: a session must be named so the base
    // directory is scoped to that session's working directory.
    let sid = match session_id {
        Some(sid) if !sid.is_empty() => sid,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "session_id is required when file_transfer.dir is not configured".to_string(),
            ));
        }
    };

    let session = state
        .session_manager
        .get_session(sid)
        .await
        .ok_or_else(|| {
            // Session not found: reject to prevent cross-session access
            (StatusCode::NOT_FOUND, format!("Session not found: {}", sid))
        })?;

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
            Ok(cwd)
        }
        Err(e) => {
            debug!(
                "Failed to read /proc/{}/cwd for session {}: {}",
                pid, sid, e
            );
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Cannot resolve working directory for session: {}", e),
            ))
        }
    }
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

/// Open a file for reading with `O_NOFOLLOW`, rejecting symlinked final
/// components. Superseded by [`open_nofollow_recursive`] for downloads,
/// but still used by unit tests to verify O_NOFOLLOW semantics.
#[cfg(test)]
fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// Resolve and open a file under `base` without ever following a symlink.
///
/// Walks each component of `relative` with `openat(O_NOFOLLOW | O_DIRECTORY)`
/// starting from a canonicalized `base`, then opens the final component with
/// `O_NOFOLLOW`. Because resolution and opening happen in a single kernel
/// walk rooted at an fd, an attacker who can create symlinks inside the base
/// (e.g. via upload) cannot swap a *middle* directory for a symlink between
/// a `canonicalize()` check and the final `open` — the TOCTOU that
/// `safe_resolve` + `open_nofollow` leaves open.
///
/// Returns the opened file plus its full path (for MIME/Content-Disposition).
fn open_nofollow_recursive(
    base: &Path,
    relative: &str,
) -> Result<(std::fs::File, PathBuf), (StatusCode, String)> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    let err = |status: StatusCode, msg: &str| -> (StatusCode, String) { (status, msg.to_string()) };

    // Reject absolute paths and traversal components up front.
    if relative.starts_with('/') {
        return Err(err(StatusCode::FORBIDDEN, "Absolute paths are not allowed"));
    }

    // Root the walk at the canonical base directory.
    let base_canon = base.canonicalize().map_err(|_| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Base directory not accessible",
        )
    })?;
    let mut dir_fd = match std::fs::File::open(&base_canon) {
        Ok(f) => f,
        Err(e) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Base directory not accessible: {}", e),
            ));
        }
    };
    let mut resolved = base_canon;

    let components: Vec<&std::ffi::OsStr> = Path::new(relative)
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(os) => Some(os),
            // "." and trailing slashes are harmless; ".." is rejected outright
            std::path::Component::ParentDir => None,
            _ => None,
        })
        .collect();
    if Path::new(relative)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(err(StatusCode::FORBIDDEN, "Path traversal detected"));
    }
    if components.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "Empty path"));
    }

    let last_idx = components.len() - 1;
    for (i, comp) in components.iter().enumerate() {
        let c_comp = std::ffi::CString::new(comp.as_bytes())
            .map_err(|_| err(StatusCode::BAD_REQUEST, "Invalid path component"))?;

        let is_last = i == last_idx;
        let flags = if is_last {
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else {
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC
        };

        // SAFETY: c_comp is a valid NUL-terminated string; dir_fd.raw_fd()
        // is an open directory fd. The returned fd is wrapped in a File so
        // it is closed on drop / error paths.
        let fd = unsafe { libc::openat(dir_fd.as_raw_fd(), c_comp.as_ptr(), flags) };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            let status = match e.raw_os_error() {
                Some(libc::ELOOP) => StatusCode::FORBIDDEN,
                Some(libc::ENOENT) => StatusCode::NOT_FOUND,
                Some(libc::ENOTDIR) => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            let msg = match e.raw_os_error() {
                Some(libc::ELOOP) => "Path is a symbolic link",
                Some(libc::ENOENT) => "Path not found",
                Some(libc::ENOTDIR) => "Path component is not a directory",
                _ => "Cannot open path",
            };
            return Err(err(status, msg));
        }
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        resolved = resolved.join(comp);
        dir_fd = file;
    }

    Ok((dir_fd, resolved))
}

/// Atomically rename `from` to `to` without replacing an existing target.
///
/// Uses `renameat2(RENAME_NOREPLACE)` on Linux. Returns
/// `ErrorKind::AlreadyExists` when the target exists, closing the TOCTOU
/// window between an `exists()` check and a plain `rename` (which would
/// otherwise silently overwrite a file created concurrently).
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let from_c = std::ffi::CString::new(from.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid path"))?;
    let to_c = std::ffi::CString::new(to.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid path"))?;

    // SAFETY: both pointers are valid NUL-terminated C strings; the syscall
    // does not retain them beyond the call.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from_c.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        return Ok(());
    }
    Err(std::io::Error::last_os_error())
}

/// Bounded drain of the remaining multipart body.
///
/// Before returning an error response we must consume the rest of the body:
/// if the server closes while the client is still uploading, the browser
/// gets a TCP RST (ERR_CONNECTION_ABORTED) instead of the error status.
/// The timeout bounds how long a slow client can hold the handler (Slowloris).
async fn drain_multipart(multipart: &mut Multipart, timeout_dur: std::time::Duration) {
    let _ = tokio::time::timeout(timeout_dur, async {
        while let Ok(Some(mut remaining)) = multipart.next_field().await {
            while let Ok(Some(_)) = remaining.chunk().await {}
        }
    })
    .await;
}

/// Drain the current field's remaining chunks (bounded).
async fn drain_field(
    field: &mut axum::extract::multipart::Field<'_>,
    timeout_dur: std::time::Duration,
) {
    let _ = tokio::time::timeout(timeout_dur, async {
        while let Ok(Some(_)) = field.chunk().await {}
    })
    .await;
}

/// POST /api/files/upload — upload a file via multipart/form-data
pub async fn upload_file(
    State(state): State<FileTransferState>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, (StatusCode, Json<FileErrorResponse>)> {
    let base = resolve_base_dir(&state, query.session_id.as_deref())
        .await
        .map_err(|(status, msg)| (status, Json(FileErrorResponse { error: msg })))?;
    let max_size = state.config.max_upload_size;

    // Fast-path rejection: if the client provided Content-Length and it
    // already exceeds the limit, reject immediately without reading any body.
    if let Some(cl) = headers.get(header::CONTENT_LENGTH)
        && let Some(len) = cl.to_str().ok().and_then(|s| s.parse::<u64>().ok())
        && len > max_size as u64
    {
        // Drain the multipart body to allow a clean connection close.
        // Without this, the browser gets ERR_CONNECTION_ABORTED because
        // the server closes while the client is still sending data.
        drain_multipart(&mut multipart, std::time::Duration::from_secs(10)).await;
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(FileErrorResponse {
                error: format!(
                    "File too large: Content-Length {} exceeds max {} bytes",
                    len, max_size
                ),
            }),
        ));
    }

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

    while let Some(mut field) = multipart.next_field().await.map_err(|e| {
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

        // Overwrite protection: reject if file exists and overwrite not requested.
        // IMPORTANT: We must drain the remaining body before returning the error
        // response. If we don't, the browser is still uploading when the server
        // closes the connection, causing a TCP RST (ERR_CONNECTION_ABORTED)
        // instead of delivering the 409 response.
        if !query.overwrite && target.exists() {
            // Drain the current field data and any remaining multipart fields
            // so the client receives the 409 response instead of a TCP RST.
            drain_field(&mut field, std::time::Duration::from_secs(10)).await;
            drop(field);
            drain_multipart(&mut multipart, std::time::Duration::from_secs(10)).await;
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

        // Stream field data to a temporary file, then atomically rename it
        // into place. The temp file:
        // - prevents concurrent uploads of the same name from interleaving
        //   writes into one corrupt file (last successful rename wins),
        // - means readers never observe a partially written file,
        // - and the rename itself replaces any symlink at the target path
        //   rather than following it (mitigates a TOCTOU on the target).
        let tmp_target = target.with_extension(format!(
            "{}.{}.tmp",
            target
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_default(),
            uuid::Uuid::new_v4()
        ));
        let mut file = tokio::fs::File::create(&tmp_target).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(FileErrorResponse {
                    error: format!("Cannot create file: {}", e),
                }),
            )
        })?;

        // RAII guard: ensures the partial temp file is removed on any error
        // path (read failure, write failure, size exceeded, connection abort)
        let mut guard = UploadFileGuard::new(tmp_target.clone());

        const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

        let mut total_size: usize = 0;
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
                // Abort: drop file handle; the guard will remove the partial file
                drop(file);
                // Drain the remaining body so the client receives the error
                // response instead of a TCP RST — bounded so a slow client
                // cannot hold the handler forever (Slowloris).
                drain_field(&mut field, DRAIN_TIMEOUT).await;
                drop(field);
                drain_multipart(&mut multipart, DRAIN_TIMEOUT).await;
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
        drop(file);

        // Atomically move the completed temp file into place. The guard is
        // still armed: if the rename fails it removes the temp file.
        //
        // When `overwrite` was not requested we use renameat2 with
        // RENAME_NOREPLACE so the "does not exist" check and the move are a
        // single atomic operation — two concurrent uploads of the same name
        // can no longer both pass an exists() check and silently clobber
        // each other. EEXIST maps to 409 Conflict.
        let rename_result = if query.overwrite {
            tokio::fs::rename(&tmp_target, &target).await
        } else {
            let tmp = tmp_target.clone();
            let dst = target.clone();
            tokio::task::spawn_blocking(move || rename_noreplace(&tmp, &dst))
                .await
                .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
        };
        if let Err(e) = rename_result {
            let (status, message) = if e.kind() == std::io::ErrorKind::AlreadyExists {
                (
                    StatusCode::CONFLICT,
                    format!(
                        "File already exists: {}. Use overwrite=true to replace.",
                        sanitized
                    ),
                )
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to finalize upload: {}", e),
                )
            };
            return Err((status, Json(FileErrorResponse { error: message })));
        }

        // Upload completed successfully — disarm the guard (the temp file no
        // longer exists; it became the target).
        guard.disarm();

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

/// Build an RFC 6266/5987-compliant `Content-Disposition` value.
///
/// Emits `filename="..."` for the (sanitized) name plus a
/// `filename*=UTF-8''...` percent-encoded form so non-ASCII filenames are
/// rendered correctly by modern clients instead of turning into mojibake.
fn content_disposition(filename: &str) -> String {
    let mut encoded = String::with_capacity(filename.len());
    for byte in filename.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        filename, encoded
    )
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

    // Resolve and open in a single kernel walk rooted at the base directory.
    // Unlike safe_resolve() + open(), this cannot be raced with a symlink
    // swapped into a *middle* path component (see open_nofollow_recursive).
    let (std_file, target) =
        open_nofollow_recursive(&base, relative).map_err(|(status, msg)| {
            (
                status,
                Json(FileErrorResponse {
                    error: if msg == "Path not found" {
                        format!("File not found: {}", relative)
                    } else {
                        msg
                    },
                }),
            )
        })?;

    let metadata = std_file.metadata().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(FileErrorResponse {
                error: format!("Cannot read file metadata: {}", e),
            }),
        )
    })?;

    // Convert to async after all synchronous metadata work is done.
    let file = tokio::fs::File::from_std(std_file);

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
    let disposition = content_disposition(&safe_name);

    let mime = mime_guess::from_path(&target).first_or_octet_stream();
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CONTENT_LENGTH, metadata.len())
        .header(header::CONTENT_DISPOSITION, disposition)
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

    #[test]
    fn test_content_disposition_ascii() {
        let cd = content_disposition("report.txt");
        assert_eq!(
            cd,
            "attachment; filename=\"report.txt\"; filename*=UTF-8''report.txt"
        );
    }

    #[test]
    fn test_content_disposition_utf8() {
        let cd = content_disposition("报告.md");
        // Non-ASCII bytes must be percent-encoded in the filename* form
        assert!(cd.contains("filename=\"报告.md\""));
        assert!(cd.contains("filename*=UTF-8''%E6%8A%A5%E5%91%8A.md",));
    }

    #[test]
    fn test_content_disposition_special_chars() {
        let cd = content_disposition("a b+c.txt");
        // Space and '+' are not in the RFC 5987 attr-char set
        assert!(cd.contains("filename*=UTF-8''a%20b%2Bc.txt"));
    }

    fn test_state(dir: PathBuf) -> FileTransferState {
        FileTransferState {
            config: Arc::new(FileTransferConfig {
                enabled: true,
                dir: Some(dir),
                max_upload_size: 1024,
                allow_unauthenticated: true,
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

    #[test]
    fn test_open_nofollow_rejects_symlink() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-nofollow");
        let _ = fs::create_dir_all(&dir);
        let secret = std::env::temp_dir().join("ttyd-rs-files-test-nofollow-secret");
        fs::write(&secret, b"secret").unwrap();

        // Symlink inside the base pointing outside it
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, dir.join("link")).unwrap();

        // O_NOFOLLOW must reject the symlinked final component (ELOOP)
        let result = open_nofollow(&dir.join("link"));
        assert!(result.is_err(), "O_NOFOLLOW must reject a symlink");

        // A regular file still opens fine
        fs::write(dir.join("regular.txt"), b"data").unwrap();
        assert!(open_nofollow(&dir.join("regular.txt")).is_ok());

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&secret);
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
    async fn test_base_dir_requires_session_when_no_config() {
        let state = FileTransferState {
            config: Arc::new(FileTransferConfig {
                enabled: true,
                dir: None,
                max_upload_size: 1024,
                allow_unauthenticated: true,
            }),
            session_manager: Arc::new(SessionManager::new(
                Duration::from_secs(3600),
                SessionMode::Isolated,
            )),
        };
        // Omitting session_id must be rejected (400): a fallback to the most
        // recently active session would allow cross-session directory access.
        let result = resolve_base_dir(&state, None).await;
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // An empty session_id is likewise rejected.
        let result = resolve_base_dir(&state, Some("")).await;
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_base_dir_invalid_session_returns_error() {
        let state = FileTransferState {
            config: Arc::new(FileTransferConfig {
                enabled: true,
                dir: None,
                max_upload_size: 1024,
                allow_unauthenticated: true,
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
                allow_unauthenticated: true,
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

    // ── Upload integration tests ─────────────────────────────────────

    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::post;
    use tower::ServiceExt;

    /// Build a multipart/form-data body for upload testing
    fn multipart_body(boundary: &str, field_name: &str, filename: &str, content: &str) -> String {
        format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"{field_name}\"; filename=\"{filename}\"\r\n\
             Content-Type: application/octet-stream\r\n\
             \r\n\
             {content}\r\n\
             --{boundary}--\r\n"
        )
    }

    /// Build a router with the upload endpoint for testing.
    /// The DefaultBodyLimit is set high so that our streaming size check
    /// (not the axum body limit) enforces the upload limit.
    fn upload_router(state: FileTransferState, max_size: usize) -> Router {
        let state_with_size = FileTransferState {
            config: Arc::new(FileTransferConfig {
                enabled: true,
                dir: state.config.dir.clone(),
                max_upload_size: max_size,
                allow_unauthenticated: true,
            }),
            session_manager: state.session_manager.clone(),
        };
        // Set body limit well above max_size so the multipart parser
        // can read the full body; our streaming check enforces the real limit.
        let body_limit = std::cmp::max(max_size, 1024 * 1024);
        Router::new()
            .route("/upload", post(upload_file).with_state(state_with_size))
            .layer(axum::extract::DefaultBodyLimit::max(body_limit))
    }

    #[tokio::test]
    async fn test_upload_success() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-upload-ok");
        let _ = fs::create_dir_all(&dir);

        let state = test_state(dir.clone());
        let app = upload_router(state, 1024 * 1024); // 1MB limit

        let boundary = "testboundary123";
        let body = multipart_body(boundary, "file", "hello.txt", "Hello, World!");
        let content_type = format!("multipart/form-data; boundary={boundary}");

        let req = Request::builder()
            .method("POST")
            .uri("/upload")
            .header("Content-Type", content_type)
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json["filename"], "hello.txt");
        assert_eq!(json["size"], 13);

        // Verify file is on disk
        let file_path = dir.join("hello.txt");
        assert!(file_path.exists());
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "Hello, World!");

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_upload_conflict() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-upload-conflict");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("existing.txt"), "old content").unwrap();

        let state = test_state(dir.clone());
        let app = upload_router(state, 1024 * 1024);

        let boundary = "testboundary456";
        let body = multipart_body(boundary, "file", "existing.txt", "new content");
        let content_type = format!("multipart/form-data; boundary={boundary}");

        let req = Request::builder()
            .method("POST")
            .uri("/upload")
            .header("Content-Type", content_type)
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert!(json["error"].as_str().unwrap().contains("already exists"));

        // Original file should be unchanged
        assert_eq!(
            fs::read_to_string(dir.join("existing.txt")).unwrap(),
            "old content"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_upload_conflict_overwrite() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-upload-ow");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("existing.txt"), "old content").unwrap();

        let state = test_state(dir.clone());
        let app = upload_router(state, 1024 * 1024);

        let boundary = "testboundary789";
        let body = multipart_body(boundary, "file", "existing.txt", "new content");
        let content_type = format!("multipart/form-data; boundary={boundary}");

        let req = Request::builder()
            .method("POST")
            .uri("/upload?overwrite=true")
            .header("Content-Type", content_type)
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // File should be overwritten
        assert_eq!(
            fs::read_to_string(dir.join("existing.txt")).unwrap(),
            "new content"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_upload_size_exceeded() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-upload-size");
        let _ = fs::create_dir_all(&dir);

        let state = test_state(dir.clone());
        // Very small limit: 10 bytes
        let app = upload_router(state, 10);

        let boundary = "testboundarysize";
        let content = "This content is way too long for the 10 byte limit";
        let body = multipart_body(boundary, "file", "big.txt", content);
        let content_type = format!("multipart/form-data; boundary={boundary}");

        let req = Request::builder()
            .method("POST")
            .uri("/upload")
            .header("Content-Type", content_type)
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert!(json["error"].as_str().unwrap().contains("too large"));

        // No partial file should remain
        assert!(!dir.join("big.txt").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_upload_no_file_field() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-upload-nofile");
        let _ = fs::create_dir_all(&dir);

        let state = test_state(dir.clone());
        let app = upload_router(state, 1024 * 1024);

        let boundary = "testboundarynofile";
        let body = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"text\"\r\n\
             \r\n\
             just text\r\n\
             --{boundary}--\r\n"
        );
        let content_type = format!("multipart/form-data; boundary={boundary}");

        let req = Request::builder()
            .method("POST")
            .uri("/upload")
            .header("Content-Type", content_type)
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert!(json["error"].as_str().unwrap().contains("No file"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_upload_binary_content() {
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-upload-bin");
        let _ = fs::create_dir_all(&dir);

        let state = test_state(dir.clone());
        let app = upload_router(state, 1024 * 1024);

        let boundary = "testboundarybin";
        let binary_content: Vec<u8> = vec![0x00, 0x01, 0x02, 0xFF, 0xFE];
        let header = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"file\"; filename=\"data.bin\"\r\n\
             Content-Type: application/octet-stream\r\n\
             \r\n"
        );
        let footer = format!("\r\n--{boundary}--\r\n");
        let mut body_bytes = header.into_bytes();
        body_bytes.extend_from_slice(&binary_content);
        body_bytes.extend_from_slice(footer.as_bytes());
        let content_type = format!("multipart/form-data; boundary={boundary}");

        let req = Request::builder()
            .method("POST")
            .uri("/upload")
            .header("Content-Type", content_type)
            .body(Body::from(body_bytes))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let file_content = fs::read(dir.join("data.bin")).unwrap();
        assert_eq!(file_content, binary_content);

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_upload_no_leftover_tmp_files() {
        // After a successful upload no *.tmp files may remain in the base dir.
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-upload-tmp-clean");
        let _ = fs::create_dir_all(&dir);

        let state = test_state(dir.clone());
        let app = upload_router(state, 1024 * 1024);

        let boundary = "testboundary123";
        let body = multipart_body(boundary, "file", "clean.txt", "data");
        let req = Request::builder()
            .method("POST")
            .uri("/upload")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover tmp files: {leftovers:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_upload_failed_size_check_leaves_original_intact() {
        // An oversized upload must not clobber an existing file with the
        // same name (atomic tmp+rename guarantees this).
        let dir = std::env::temp_dir().join("ttyd-rs-files-test-upload-atomic");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("keep.txt"), "original").unwrap();

        let state = test_state(dir.clone());
        let app = upload_router(state.clone(), 8); // tiny limit

        let boundary = "testboundary123";
        let body = multipart_body(
            boundary,
            "file",
            "fresh.txt",
            "this content is way too long",
        );
        let req = Request::builder()
            .method("POST")
            .uri("/upload")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);

        // The original file must be untouched and no tmp leftovers remain.
        assert_eq!(
            fs::read_to_string(dir.join("keep.txt")).unwrap(),
            "original"
        );
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover tmp files: {leftovers:?}");

        let _ = fs::remove_dir_all(&dir);
    }
}
