// ============================================================
// status.js — server status quick-view panel.
// Fetches GET /api/status (with credentials when auth is
// configured) and renders it in the header dropdown, refreshing
// while the panel is open.
// ============================================================

import { Auth } from './auth.js';

const REFRESH_INTERVAL = 5000;

const panel = document.getElementById('status-panel');
const btnStatus = document.getElementById('btn-status');
const statusWrapper = document.getElementById('status-wrapper');
const versionEl = document.getElementById('status-version');
const uptimeEl = document.getElementById('status-uptime');
const connectionsEl = document.getElementById('status-connections');
const sessionsTotalEl = document.getElementById('status-sessions-total');
const sessionsSplitEl = document.getElementById('status-sessions-split');
const sessionsClientsEl = document.getElementById('status-sessions-clients');
const appMemoryEl = document.getElementById('status-app-memory');
const appCpuEl = document.getElementById('status-app-cpu');
const hostMemoryEl = document.getElementById('status-host-memory');
const hostCpuEl = document.getElementById('status-host-cpu');
const hostLoadEl = document.getElementById('status-host-load');
const hostUptimeEl = document.getElementById('status-host-uptime');
const hostKernelEl = document.getElementById('status-host-kernel');

/** @type {number | null} */
let refreshTimer = null;

/** @param {string} text — show an inline error line in the panel. */
function showError(text) {
    let err = panel.querySelector('.status-error');
    if (!err) {
        err = document.createElement('div');
        err.className = 'status-error';
        panel.prepend(err);
    }
    err.textContent = text;
}

function clearError() {
    panel.querySelector('.status-error')?.remove();
}

/** @param {number} seconds @returns {string} */
function formatUptime(seconds) {
    const d = Math.floor(seconds / 86400);
    const h = Math.floor((seconds % 86400) / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    const s = Math.floor(seconds % 60);
    if (d > 0) return `${d}d ${h}h ${m}m`;
    if (h > 0) return `${h}h ${m}m`;
    if (m > 0) return `${m}m ${s}s`;
    return `${s}s`;
}

/** @param {number} bytes @returns {string} */
function formatBytes(bytes) {
    if (bytes >= 1024 * 1024 * 1024) return `${(bytes / (1024 ** 3)).toFixed(1)} GB`;
    if (bytes >= 1024 * 1024) return `${(bytes / (1024 ** 2)).toFixed(1)} MB`;
    return `${(bytes / 1024).toFixed(0)} kB`;
}

/** @param {number} percent 0..100 @returns {string} */
function formatPercent(percent) {
    return `${percent.toFixed(1)}%`;
}

/**
 * @param {object} status — the /api/status payload
 */
function render(status) {
    versionEl.textContent = status.version ?? '–';
    uptimeEl.textContent = formatUptime(status.uptime_seconds ?? 0);
    connectionsEl.textContent =
        `${status.connections?.active ?? 0} / ${status.connections?.max ?? 0}`;

    sessionsTotalEl.textContent = String(status.sessions?.total ?? 0);
    sessionsSplitEl.textContent =
        `${status.sessions?.isolated ?? 0} / ${status.sessions?.shared ?? 0}`;
    sessionsClientsEl.textContent = String(status.sessions?.clients ?? 0);

    // Application: the ttyd-rs process itself.
    const app = status.application ?? {};
    appMemoryEl.textContent = app.rss_bytes != null ? formatBytes(app.rss_bytes) : 'n/a';
    // Process CPU can legitimately exceed 100% across cores.
    appCpuEl.textContent = app.cpu_percent != null ? formatPercent(app.cpu_percent) : 'n/a';

    // Server: host-level metrics.
    const srv = status.server ?? {};
    const mem = srv.memory;
    if (mem && mem.total_bytes > 0) {
        const usedPct = (mem.used_bytes / mem.total_bytes) * 100;
        hostMemoryEl.textContent =
            `${formatBytes(mem.used_bytes)} / ${formatBytes(mem.total_bytes)} (${usedPct.toFixed(0)}%)`;
    } else {
        hostMemoryEl.textContent = 'n/a';
    }
    hostCpuEl.textContent = srv.cpu_percent != null ? formatPercent(srv.cpu_percent) : 'n/a';
    hostLoadEl.textContent = Array.isArray(srv.load_average)
        ? srv.load_average.map((v) => v.toFixed(2)).join(' ')
        : 'n/a';
    hostUptimeEl.textContent = srv.uptime_seconds != null
        ? formatUptime(srv.uptime_seconds)
        : 'n/a';
    hostKernelEl.textContent = srv.kernel ?? 'n/a';
}

/** Fetch once and re-render; on failure show an inline error. */
async function refresh() {
    try {
        const headers = {};
        const authHeader = Auth.httpHeader();
        if (authHeader) headers.Authorization = authHeader;

        const resp = await fetch('/api/status', { headers });
        if (!resp.ok) {
            throw new Error(`HTTP ${resp.status}`);
        }
        clearError();
        render(await resp.json());
    } catch (e) {
        console.error('Failed to fetch server status:', e);
        showError('Status unavailable — retrying…');
    }
}

export function isStatusPanelOpen() {
    return panel.classList.contains('open');
}

export function openStatusPanel() {
    panel.classList.add('open');
    btnStatus.setAttribute('aria-expanded', 'true');
    refresh();
    if (refreshTimer === null) {
        refreshTimer = setInterval(refresh, REFRESH_INTERVAL);
    }
}

export function closeStatusPanel() {
    panel.classList.remove('open');
    btnStatus.setAttribute('aria-expanded', 'false');
    if (refreshTimer !== null) {
        clearInterval(refreshTimer);
        refreshTimer = null;
    }
}

/** Reveal the status button once the user is authenticated. */
export function showStatusButton() {
    statusWrapper.classList.remove('hidden');
}

/**
 * Wire the panel toggle. Call once after DOM ready.
 * @param {() => void} closeOtherMenus — hide sibling dropdowns
 */
export function initStatusPanel(closeOtherMenus) {
    btnStatus.addEventListener('click', (e) => {
        e.stopPropagation();
        const willOpen = !isStatusPanelOpen();
        closeOtherMenus();
        if (willOpen) openStatusPanel();
        else closeStatusPanel();
    });
}
