// ============================================================
// files.js — file browser panel (breadcrumb, listing, loading).
// ============================================================

import { CONFIG, Prefs } from './config.js';
import { ICONS } from './icons.js';
import { formatSize, triggerDownload } from './transfer.js';

const filePanelOverlay = document.getElementById('file-panel-overlay');
const filePanelBody = document.getElementById('file-panel-body');
const filePanelClose = document.getElementById('file-panel-close');
const filePanelRefresh = document.getElementById('file-panel-refresh');
const filePanelBreadcrumb = document.getElementById('file-panel-breadcrumb');
const showHiddenToggle = document.getElementById('show-hidden-toggle');

// Restore persisted "show hidden" preference (default: unchecked)
showHiddenToggle.checked = Prefs.getBool(CONFIG.storage.KEYS.SHOW_HIDDEN, false);

let currentFilePath = '.';
let loadingTimer = null;

/** @type {(path: string) => void} set by init() — sends the file_list WS message */
let requestList = () => {};

showHiddenToggle.addEventListener('change', () => {
    // Persist the "show hidden" preference, then refresh the listing
    Prefs.set(CONFIG.storage.KEYS.SHOW_HIDDEN, String(showHiddenToggle.checked));
    showLoading();
    requestList(currentFilePath);
});

filePanelClose.addEventListener('click', () => {
    filePanelOverlay.classList.add('hidden');
});

filePanelRefresh.addEventListener('click', () => {
    showLoading();
    requestList(currentFilePath);
});

filePanelOverlay.addEventListener('click', (e) => {
    if (e.target === filePanelOverlay) {
        filePanelOverlay.classList.add('hidden');
    }
});

// Show the loading spinner, but only after a short delay. If the
// listing arrives quickly we keep the previous content and render the
// new list directly — this avoids the spinner flashing for a few ms
// (the "flicker" seen on fast loads). Slow loads still get feedback.
function showLoading() {
    if (loadingTimer !== null) return; // already pending
    loadingTimer = setTimeout(() => {
        loadingTimer = null;
        filePanelBody.innerHTML = '<div class="fp-loading"><div class="spinner"></div>Loading...</div>';
    }, CONFIG.ui.LOADING_DELAY);
}

export function cancelLoading() {
    if (loadingTimer !== null) {
        clearTimeout(loadingTimer);
        loadingTimer = null;
    }
}

function renderBreadcrumb(path) {
    filePanelBreadcrumb.innerHTML = '';
    const rootCrumb = document.createElement('span');
    rootCrumb.className = 'crumb';
    rootCrumb.textContent = '~';
    rootCrumb.addEventListener('click', () => {
        requestFileList('.');
    });
    filePanelBreadcrumb.appendChild(rootCrumb);

    if (path === '.' || path === '') return;
    const parts = path.split('/').filter(Boolean);
    let accum = '';
    parts.forEach((part) => {
        accum = accum ? accum + '/' + part : part;
        const sep = document.createElement('span');
        sep.className = 'sep';
        sep.textContent = '/';
        filePanelBreadcrumb.appendChild(sep);

        const crumb = document.createElement('span');
        crumb.className = 'crumb';
        crumb.textContent = part;
        const target = accum;
        crumb.addEventListener('click', () => {
            requestFileList(target);
        });
        filePanelBreadcrumb.appendChild(crumb);
    });
}

function requestFileList(path) {
    currentFilePath = path || '.';
    renderBreadcrumb(currentFilePath);
    showLoading();
    requestList(currentFilePath);
}

export function openFilePanel() {
    filePanelOverlay.classList.remove('hidden');
    requestFileList('.');
    requestAnimationFrame(() => filePanelClose.focus());
}

export function isFilePanelOpen() {
    return !filePanelOverlay.classList.contains('hidden');
}

/** Current "show hidden files" preference (used when sending file_list). */
export function showHiddenEnabled() {
    return showHiddenToggle.checked;
}

/** Render a "not connected" placeholder in the panel body. */
export function renderNotConnected() {
    filePanelBody.innerHTML = '<div class="fp-empty">Not connected</div>';
}

export function hideFilePanel() {
    filePanelOverlay.classList.add('hidden');
}

function joinPath(base, name) {
    if (base === '.' || base === '') return name;
    return base + '/' + name;
}

function parentPath(path) {
    if (path === '.' || path === '') return null;
    const idx = path.lastIndexOf('/');
    return idx > 0 ? path.substring(0, idx) : '.';
}

export function renderFileList(data) {
    cancelLoading();
    const files = data.entries || [];
    filePanelBody.innerHTML = '';

    // ".." entry to go up
    const parent = parentPath(currentFilePath);
    if (parent !== null) {
        const upItem = document.createElement('div');
        upItem.className = 'file-item is-parent';
        const upIcon = document.createElement('span');
        upIcon.className = 'file-icon';
        upIcon.innerHTML = ICONS.up;
        const upName = document.createElement('span');
        upName.className = 'file-item-name';
        upName.textContent = '..';
        upItem.appendChild(upIcon);
        upItem.appendChild(upName);
        upItem.addEventListener('click', () => {
            requestFileList(parent);
        });
        filePanelBody.appendChild(upItem);
    }

    if (files.length === 0 && parent === null) {
        filePanelBody.innerHTML =
            '<div class="fp-empty"><div class="empty-icon">&#128193;</div>No files in current directory</div>';
        return;
    }

    files.forEach(f => {
        const item = document.createElement('div');
        item.className = 'file-item' + (f.is_dir ? ' is-dir' : '');

        const icon = document.createElement('span');
        icon.className = 'file-icon';
        icon.innerHTML = f.is_dir ? ICONS.dir : ICONS.file;

        const name = document.createElement('span');
        name.className = 'file-item-name';
        name.textContent = f.name;
        name.title = f.name;

        const meta = document.createElement('span');
        meta.className = 'file-item-meta';
        meta.textContent = f.is_dir ? 'DIR' : formatSize(f.size);

        item.appendChild(icon);
        item.appendChild(name);
        item.appendChild(meta);

        if (f.is_dir) {
            item.addEventListener('click', () => {
                requestFileList(joinPath(currentFilePath, f.name));
            });
        } else {
            const dl = document.createElement('button');
            dl.className = 'file-item-dl';
            dl.textContent = 'Download';
            dl.addEventListener('click', (e) => {
                e.stopPropagation();
                triggerDownload(joinPath(currentFilePath, f.name));
            });
            item.appendChild(dl);
        }

        filePanelBody.appendChild(item);
    });
}

/** Show a server-side file_list error inside the panel. */
export function renderFileListError(message) {
    cancelLoading();
    filePanelBody.innerHTML = `<div class="fp-empty">${escapeHtml(message)}</div>`;
}

/**
 * Wire the panel to the WS layer.
 * @param {(path: string) => void} sendFileList
 */
export function initFilePanel(sendFileList) {
    requestList = sendFileList;
}