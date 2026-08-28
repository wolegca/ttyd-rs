// ============================================================
// transfer.js — file upload/download queue, transfer progress
// panel, and shared helpers (formatSize, escapeHtml).
// ============================================================

import { CONFIG } from './config.js';
import { ICONS } from './icons.js';
import { Auth } from './auth.js';
import { showToast, showConfirm } from './toast.js';

// Injected by main.js to avoid circular imports.
let getSessionId = () => null;
let closeMenus = () => {};

/** @param {() => string|null} getter */
export function initTransfer(getSessionIdFn, closeMenusFn) {
    getSessionId = getSessionIdFn;
    closeMenus = closeMenusFn;
}

// ============================================================
// Shared helpers
// ============================================================

/**
 * Format byte size to human-readable string.
 * @param {number} bytes
 * @returns {string}
 */
export function formatSize(bytes) {
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
    if (bytes < 1024 * 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
    return (bytes / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
}

/** Escape a string for safe insertion into HTML.
 *  @param {string} s
 *  @returns {string}
 */
export function escapeHtml(s) {
    // Entity strings are built from char codes so they can never be
    // altered by any HTML-entity decoding applied to this file.
    const ENT = {
        '&': String.fromCharCode(38, 97, 109, 112, 59),      // amp
        '<': String.fromCharCode(38, 108, 116, 59),          // lt
        '>': String.fromCharCode(38, 103, 116, 59),          // gt
        '"': String.fromCharCode(38, 113, 117, 111, 116, 59), // quot
        "'": String.fromCharCode(38, 35, 51, 57, 59)         // apos
    };
    return String(s).replace(/[&<>"']/g, (c) => ENT[c]);
}

// ============================================================
// Transfer state (uploads and downloads share one queue/panel)
// ============================================================
const uploadIndicator = document.getElementById('upload-indicator');
const btnUploadIndicator = document.getElementById('btn-upload-indicator');
const uploadPanel = document.getElementById('upload-panel');
const uploadPanelTitle = document.getElementById('upload-panel-title');
const uploadPanelList = document.getElementById('upload-panel-list');
const btnCancelAll = document.getElementById('btn-cancel-all');
const ringFill = btnUploadIndicator.querySelector('.ring-fill');
const ringText = btnUploadIndicator.querySelector('.ring-text');

/**
 * Transfer items shown in the indicator panel (uploads and downloads).
 * Upload items carry `file`; download items carry `name` and `total` bytes.
 * @type {Array<{id: number, kind: 'upload'|'download', name: string, file?: File,
 *               state: 'queued'|'uploading'|'done'|'failed'|'cancelled', pct: number,
 *               total?: number, controller?: AbortController}>}
 */
const transferItems = [];
let transferItemId = 0;
/** XHR of the currently active upload, if any */
let activeUploadXhr = null;
/** Set when the user cancels the active upload so its error handler stays quiet */
let uploadCancelled = false;
/** Server-side file listing cache used for pre-flight existence checks. */
let serverFileCache = null;

/** Whether an upload is currently in flight. */
function uploadBusy() {
    return transferItems.some((i) => i.state === 'uploading' && i.kind === 'upload');
}

/** Compute overall progress (0-100) across all transfer items. */
function computeOverallProgress() {
    if (transferItems.length === 0) return 0;
    return transferItems.reduce((sum, i) => sum + (i.state === 'done' ? 100 : i.pct), 0) / transferItems.length;
}

function renderTransferPanel() {
    uploadPanelList.innerHTML = '';
    for (const item of transferItems) {
        const row = document.createElement('div');
        row.className = 'upload-row ' + item.state;

        const name = document.createElement('span');
        name.className = 'upload-name';
        name.textContent = item.name;
        name.title = item.name;

        const status = document.createElement('span');
        status.className = 'upload-status';
        if (item.state === 'uploading') {
            status.textContent = item.pct + '%';
        } else if (item.state === 'queued') {
            status.textContent = 'queued';
        } else if (item.state === 'done') {
            status.textContent = '✓';
            status.classList.add('done');
        } else if (item.state === 'failed') {
            status.textContent = 'failed';
            status.classList.add('failed');
        } else {
            status.textContent = 'cancelled';
            status.classList.add('cancelled');
        }

        row.appendChild(name);
        row.appendChild(status);

        if (item.state === 'uploading' || item.state === 'queued') {
            const cancel = document.createElement('button');
            cancel.className = 'upload-cancel';
            cancel.title = 'Cancel ' + item.kind;
            cancel.setAttribute('aria-label', 'Cancel ' + item.kind + ' of ' + item.name);
            cancel.innerHTML = ICONS.close;
            cancel.addEventListener('click', (e) => {
                e.stopPropagation();
                cancelTransferItem(item.id);
            });
            row.appendChild(cancel);
        }

        uploadPanelList.appendChild(row);
    }

    // Header summary
    const active = transferItems.filter((i) => i.state === 'uploading' || i.state === 'queued');
    const done = transferItems.filter((i) => i.state === 'done').length;
    const hasDownload = transferItems.some((i) => i.kind === 'download');
    const verb = hasDownload ? 'Transferring' : 'Uploading';
    if (active.length > 0) {
        const current = transferItems.find((i) => i.state === 'uploading');
        const overall = Math.round(computeOverallProgress());
        uploadPanelTitle.textContent = transferItems.length > 1
            ? `${verb} ${done + 1}/${transferItems.length} · ${overall}%`
            : `${verb} · ${overall}%`;
        if (current) uploadPanelTitle.title = current.name;
    } else {
        const failed = transferItems.filter((i) => i.state === 'failed' || i.state === 'cancelled').length;
        uploadPanelTitle.textContent = failed > 0
            ? `Finished · ${done} ok, ${failed} cancelled/failed`
            : `Finished · ${done} transferred`;
    }
    btnCancelAll.style.display = active.length > 0 ? '' : 'none';

    // Ring progress + center task count
    ringFill.style.strokeDashoffset = String(100 - computeOverallProgress());
    ringText.textContent = String(transferItems.length);

    // Icon visibility
    if (transferItems.length === 0) {
        uploadIndicator.classList.add('hidden');
        uploadPanel.classList.remove('open');
    } else {
        uploadIndicator.classList.remove('hidden');
    }
}

function setUploadProgress(percent) {
    const current = transferItems.find((i) => i.state === 'uploading' && i.kind === 'upload');
    if (current) {
        current.pct = percent;
        renderTransferPanel();
    }
}

/**
 * Cancel a single queued/active transfer by id.
 * @param {number} id
 */
function cancelTransferItem(id) {
    const item = transferItems.find((i) => i.id === id);
    if (!item) return;
    if (item.state === 'uploading') {
        if (item.kind === 'upload') {
            uploadCancelled = true;
            if (activeUploadXhr) activeUploadXhr.abort();
        } else if (item.controller) {
            item.controller.abort();
        }
    } else if (item.state === 'queued') {
        item.state = 'cancelled';
        renderTransferPanel();
    }
}

/** Cancel everything: abort active transfers and drop the queue. */
function cancelAllTransfers() {
    uploadCancelled = true;
    if (activeUploadXhr) activeUploadXhr.abort();
    for (const item of transferItems) {
        if (item.state === 'queued') {
            item.state = 'cancelled';
        } else if (item.state === 'uploading' && item.kind === 'download' && item.controller) {
            item.controller.abort();
        }
    }
    renderTransferPanel();
}

btnCancelAll.addEventListener('click', (e) => {
    e.stopPropagation();
    cancelAllTransfers();
});

btnUploadIndicator.addEventListener('click', (e) => {
    e.stopPropagation();
    const willOpen = !uploadPanel.classList.contains('open');
    closeMenus();
    if (willOpen) uploadPanel.classList.add('open');
});

// Keep finished items visible briefly, then auto-hide
let transferHideTimer = null;
function scheduleTransferHide() {
    if (transferHideTimer) clearTimeout(transferHideTimer);
    transferHideTimer = setTimeout(() => {
        transferItems.length = 0;
        transferItemId = 0;
        renderTransferPanel();
    }, CONFIG.ui.UPLOAD_HIDE_DELAY);
}

// ============================================================
// Upload
// ============================================================
const btnUpload = document.getElementById('btn-upload');
const uploadInput = document.getElementById('upload-input');

/** Maximum upload size in bytes (from server config, null if not set) */
let maxUploadSize = null;

/** @param {object} config response from /api/config */
export function setServerConfig(config) {
    maxUploadSize = config.max_upload_size || null;
}

/** Queue files for sequential upload.
 *  @param {File[]|FileList} files
 */
export function queueUploads(files) {
    const list = Array.from(files);
    if (list.length === 0) return;
    if (transferHideTimer) clearTimeout(transferHideTimer);
    for (const file of list) {
        transferItems.push({ id: ++transferItemId, kind: 'upload', name: file.name, file, state: 'queued', pct: 0 });
    }
    renderTransferPanel();
    showToast(
        list.length > 1 ? `Uploading ${list.length} files` : `Uploading: ${list[0].name}`,
        'info',
    );
    // Pre-fetch the server listing once for the whole batch, so N queued
    // files don't trigger N /api/files/list requests.
    refreshServerFileCache().finally(() => {
        if (!uploadBusy()) {
            processUploadQueue();
        }
    });
}

function processUploadQueue() {
    const next = transferItems.find((i) => i.state === 'queued');
    if (!next) {
        if (transferItems.length > 0) scheduleTransferHide();
        return;
    }
    uploadFile(next);
}

/** Fetch (or re-fetch) the cached server file listing for existence checks. */
async function refreshServerFileCache() {
    serverFileCache = null;
    try {
        const url = '/api/files/list?session_id=' + encodeURIComponent(getSessionId() || '');
        const headers = {};
        const authHeader = Auth.httpHeader();
        if (authHeader) headers['Authorization'] = authHeader;
        const resp = await fetch(url, { headers });
        if (!resp.ok) return;
        const data = await resp.json();
        serverFileCache = new Set(data.entries.map((e) => e.name));
    } catch {
        // If we can't check, let the server's 409 flow handle conflicts.
    }
}

/**
 * Check if a file already exists on the server (using the cached listing).
 * @param {string} filename
 * @returns {boolean|null} true/false, or null when the cache is unavailable
 */
function fileExistsInCache(filename) {
    if (!serverFileCache) return null;
    return serverFileCache.has(filename);
}

/**
 * Upload a file with progress tracking and cancel support.
 * @param {{id: number, file: File, state: string, pct: number}} item
 * @param {boolean} [overwrite=false]
 */
async function uploadFile(item, overwrite = false) {
    const file = item.file;

    // Pre-flight: check file size against server limit
    if (maxUploadSize && file.size > maxUploadSize) {
        showToast(`${file.name}: file size (${formatSize(file.size)}) exceeds limit (${formatSize(maxUploadSize)})`, 'error');
        item.state = 'failed';
        renderTransferPanel();
        processUploadQueue();
        return;
    }

    // Pre-flight: check if file already exists to avoid ERR_CONNECTION_ABORTED
    if (!overwrite) {
        const exists = fileExistsInCache(file.name);
        if (item.state === 'cancelled') { processUploadQueue(); return; }
        if (exists) {
            const ok = await showConfirm(`"${file.name}" already exists. Overwrite it?`);
            if (ok) {
                uploadFile(item, true);
            } else {
                item.state = 'cancelled';
                renderTransferPanel();
                processUploadQueue();
            }
            return;
        }
    }

    item.state = 'uploading';
    item.pct = 0;
    renderTransferPanel();

    const xhr = new XMLHttpRequest();
    activeUploadXhr = xhr;
    uploadCancelled = false;
    const formData = new FormData();
    formData.append('file', file);

    xhr.upload.addEventListener('progress', (e) => {
        if (e.lengthComputable) {
            const pct = Math.round((e.loaded / e.total) * 100);
            setUploadProgress(pct);
        }
    });

    const finish = () => {
        activeUploadXhr = null;
        processUploadQueue();
    };

    xhr.addEventListener('load', () => {
        if (xhr.status >= 200 && xhr.status < 300) {
            item.state = 'done';
            item.pct = 100;
            renderTransferPanel();
            try {
                const data = JSON.parse(xhr.responseText);
                showToast(`Uploaded: ${data.filename} (${formatSize(data.size)})`, 'success');
            } catch (_) {
                showToast('Upload complete', 'success');
            }
            finish();
        } else if (xhr.status === 409) {
            showConfirm(`"${file.name}" already exists. Overwrite it?`).then((ok) => {
                if (ok) {
                    uploadFile(item, true);
                } else {
                    item.state = 'cancelled';
                    renderTransferPanel();
                    finish();
                }
            });
        } else {
            item.state = 'failed';
            renderTransferPanel();
            let errMsg = `Upload failed (${xhr.status})`;
            try {
                const err = JSON.parse(xhr.responseText);
                errMsg = err.error || errMsg;
            } catch (_) {}
            showToast(`${file.name}: ${errMsg}`, 'error');
            finish();
        }
    });

    xhr.addEventListener('abort', () => {
        item.state = 'cancelled';
        renderTransferPanel();
        showToast(`${file.name}: upload cancelled`, 'info');
        finish();
    });

    xhr.addEventListener('error', () => {
        if (uploadCancelled) return; // handled by the abort handler
        item.state = 'failed';
        renderTransferPanel();
        showToast(`${file.name}: upload failed (network error)`, 'error');
        finish();
    });

    let url = '/api/files/upload?session_id=' + encodeURIComponent(getSessionId() || '');
    if (overwrite) url += '&overwrite=true';
    xhr.open('POST', url);
    const authHeader = Auth.httpHeader();
    if (authHeader) xhr.setRequestHeader('Authorization', authHeader);
    xhr.send(formData);
}

btnUpload.addEventListener('click', () => {
    uploadInput.click();
});

uploadInput.addEventListener('change', () => {
    if (!uploadInput.files || uploadInput.files.length === 0) return;
    queueUploads(uploadInput.files);
    uploadInput.value = '';
});

// ============================================================
// Download (streaming, with progress + cancel)
// ============================================================

/**
 * Download a file with streaming progress, cancel support, and low
 * memory usage (chunks are collected as an array, not one big blob).
 * @param {string} filename
 */
export function triggerDownload(filename) {
    const url = '/api/files/download?path=' + encodeURIComponent(filename) + '&session_id=' + encodeURIComponent(getSessionId() || '');
    const headers = {};
    const authHeader = Auth.httpHeader();
    if (authHeader) headers['Authorization'] = authHeader;

    const item = {
        id: ++transferItemId,
        kind: 'download',
        name: filename,
        state: 'uploading',
        pct: 0,
        total: 0,
        controller: new AbortController(),
    };
    transferItems.push(item);
    if (transferHideTimer) clearTimeout(transferHideTimer);
    renderTransferPanel();
    showToast(`Downloading: ${filename}`, 'info');

    const finish = () => {
        item.controller = null;
        renderTransferPanel();
        if (!transferItems.some((i) => i.state === 'uploading' || i.state === 'queued')) {
            scheduleTransferHide();
        }
    };

    fetch(url, { headers, signal: item.controller.signal })
        .then(async (resp) => {
            if (!resp.ok) throw new Error(`Download failed (${resp.status})`);

            const total = Number(resp.headers.get('Content-Length')) || 0;
            item.total = total;

            // Stream the body in chunks to avoid buffering the whole
            // file in memory as a single Blob.
            const chunks = [];
            let received = 0;
            if (resp.body && resp.body.getReader) {
                const reader = resp.body.getReader();
                for (;;) {
                    const { done, value } = await reader.read();
                    if (done) break;
                    chunks.push(value);
                    received += value.length;
                    item.pct = total > 0 ? Math.round((received / total) * 100) : 0;
                    renderTransferPanel();
                }
            } else {
                // Older browsers without streaming: fall back to blob()
                const blob = await resp.blob();
                chunks.push(new Uint8Array(await blob.arrayBuffer()));
            }

            const blob = new Blob(chunks);
            const blobUrl = URL.createObjectURL(blob);
            const a = document.createElement('a');
            a.href = blobUrl;
            a.download = filename;
            document.body.appendChild(a);
            a.click();
            document.body.removeChild(a);
            URL.revokeObjectURL(blobUrl);

            item.state = 'done';
            item.pct = 100;
            showToast(`Downloaded: ${filename}`, 'success');
            finish();
        })
        .catch((err) => {
            if (err && err.name === 'AbortError') {
                item.state = 'cancelled';
                showToast(`Download cancelled: ${filename}`, 'info');
            } else {
                item.state = 'failed';
                showToast(err.message || 'Download failed', 'error');
            }
            finish();
        });
}

// ============================================================
// Drag & Drop Upload
// ============================================================
const dropOverlay = document.getElementById('drop-overlay');
let dragCounter = 0;

document.addEventListener('dragenter', (e) => {
    e.preventDefault();
    dragCounter++;
    if (dragCounter === 1) {
        dropOverlay.classList.remove('hidden');
    }
});

document.addEventListener('dragleave', (e) => {
    e.preventDefault();
    dragCounter--;
    if (dragCounter <= 0) {
        dragCounter = 0;
        dropOverlay.classList.add('hidden');
    }
});

document.addEventListener('dragover', (e) => {
    e.preventDefault();
});

document.addEventListener('drop', (e) => {
    e.preventDefault();
    dragCounter = 0;
    dropOverlay.classList.add('hidden');
    const files = e.dataTransfer.files;
    if (files && files.length > 0) {
        queueUploads(files);
    }
});
