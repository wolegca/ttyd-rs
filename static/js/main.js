// ============================================================
// main.js — WebSocket connection, reconnection policy, message
// dispatch, menus, and app startup.
// ============================================================

import { CONFIG } from './config.js';
import { Auth } from './auth.js';
import { term, fitAddon, writeSystemMessage, writeErrorMessage, initMobileKeys, onFontSizeChange } from './terminal.js';
import { isConfirmOpen, cancelConfirm, showToast } from './toast.js';
import { initTransfer, setServerConfig } from './transfer.js';
import {
    openFilePanel, isFilePanelOpen, hideFilePanel, cancelLoading,
    renderFileList, renderFileListError, initFilePanel,
    showHiddenEnabled, renderNotConnected,
} from './files.js';

// ============================================================
// DOM references
// ============================================================
const statusIndicator = document.getElementById('status-indicator');
const statusText = document.getElementById('status-text');
const sessionInfo = document.getElementById('session-info');
const connBanner = document.getElementById('conn-banner');
const bannerSpinner = document.getElementById('banner-spinner');
const bannerText = document.getElementById('banner-text');
const btnReconnect = document.getElementById('btn-reconnect');
const readonlyBadge = document.getElementById('readonly-badge');
const terminalContainer = document.getElementById('terminal-container');

let currentSessionId = null;

// ============================================================
// Status + banner helpers
// ============================================================
/** @param {boolean} connected */
function setStatus(connected) {
    statusIndicator.classList.remove('pending', 'reconnecting', 'disconnected');
    if (connected) {
        statusIndicator.classList.add('connected');
        statusText.textContent = 'Connected';
    } else {
        statusIndicator.classList.add('disconnected');
        statusText.textContent = 'Disconnected';
    }
}

/** Show the non-intrusive connection banner.
 *  @param {'reconnecting'|'lost'} kind
 *  @param {string} text
 */
function showBanner(kind, text) {
    connBanner.classList.remove('hidden');
    connBanner.classList.remove('reconnecting', 'lost');
    connBanner.classList.add(kind);
    bannerSpinner.classList.toggle('hidden', kind === 'lost');
    btnReconnect.style.display = kind === 'lost' ? 'inline-block' : 'none';
    bannerText.textContent = text;
    // Reflect "reconnecting" in the header indicator too
    statusIndicator.classList.remove('disconnected', 'connected', 'pending');
    statusIndicator.classList.add(kind === 'reconnecting' ? 'reconnecting' : 'disconnected');
    // Dim the terminal behind the banner
    terminalContainer.classList.add('banner-active');
}

function hideBanner() {
    connBanner.classList.add('hidden');
    terminalContainer.classList.remove('banner-active');
}

/** @param {string} sessionId */
function updateSessionInfo(sessionId) {
    currentSessionId = sessionId;
    if (sessionId) {
        sessionInfo.textContent = 'Session ' + sessionId.substring(0, 8);
    }
}

function clearSessionInfo() {
    currentSessionId = null;
    sessionInfo.textContent = '';
}

/** @param {boolean} isReadOnly */
function setReadOnlyBadge(isReadOnly) {
    readonlyBadge.classList.toggle('visible', isReadOnly);
}

// ============================================================
// Reconnect policy — retry count + exponential backoff in one place
// ============================================================
const ReconnectPolicy = {
    count: 0,
    get exhausted() { return this.count >= CONFIG.reconnect.MAX_RETRIES; },
    reset() { this.count = 0; },
    /** Record an attempt and return the delay (ms) before the next one. */
    nextDelay() {
        this.count++;
        const { BASE_DELAY, MAX_DELAY } = CONFIG.reconnect;
        return Math.min(BASE_DELAY * Math.pow(2, this.count - 1), MAX_DELAY);
    },
};

// ============================================================
// WebSocket connection with reconnection support
// ============================================================
/** @type {WebSocket | null} */
let ws = null;
/** @type {number | null} */
let reconnectTimer = null;
/** @type {number | null} */
let heartbeatTimer = null;
/** @type {number | null} */
let pongCheckTimer = null;
let lastPongTime = Date.now();
// Timestamp of the most recent transition to document.hidden === true.
// Used to make pongCheckTimer immune to the tab-freeze race: if the
// tab went hidden more recently than our last pong, `elapsed` in
// pongCheckTimer reflects wall-clock time that includes a frozen
// stretch, not real silence from the server, so it must not be
// trusted until the foreground probe re-validates. This check is
// ordering-independent - it doesn't matter whether pongCheckTimer's
// post-freeze catch-up tick fires before or after the
// visibilitychange-to-visible handler runs, because lastHiddenAt was
// already recorded earlier, when the tab *went* hidden.
let lastHiddenAt = 0;
// True while we're waiting on a response to the foreground probe
// ping (see the visibilitychange handler below). Using a flag
// instead of comparing Date.now() timestamps avoids a same-
// millisecond false positive if the pong round-trip completes
// within 1ms (e.g. on localhost).
let probeAwaitingPong = false;
// One-off timer used to verify the connection right after the tab
// becomes visible again (see the visibilitychange handler below).
// Kept separate from heartbeatTimer/pongCheckTimer because those are
// setInterval-based and, after being throttled or fully frozen by
// the browser while backgrounded, their next tick time is
// unpredictable - not suitable for a "check right now" probe.
/** @type {number | null} */
let visibilityProbeTimer = null;
let serverDisconnected = false;
let authFailed = false;
let hasAuthenticated = false;
let readOnly = false;
// True while a reconnect attempt is starting up (used to debounce
// the Enter-to-reconnect path in term.onData).
let reconnecting = false;

const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
const wsUrl = `${protocol}//${window.location.host}/ws`;

const urlParams = new URLSearchParams(window.location.search);
const joinSessionId = urlParams.get('session');

/** Safely send a JSON message over the WebSocket.
 *  @param {string} type
 *  @param {object} data
 */
function sendMsg(type, data) {
    if (ws && ws.readyState === WebSocket.OPEN) {
        try {
            ws.send(JSON.stringify({ type, data }));
        } catch (e) {
            console.error('Failed to send message:', e);
            showToast('消息发送失败，连接可能已断开', 'error');
        }
    }
}

/** Compute current terminal dimensions and send a resize (if connected). */
function sendResize() {
    if (serverDisconnected) return;
    const dims = fitAddon.proposeDimensions();
    if (dims) {
        sendMsg('resize', { cols: dims.cols, rows: dims.rows });
    }
}

function connect() {
    // Cancel any pending reconnect timer and close the old socket
    if (reconnectTimer !== null) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
    }
    authFailed = false;
    if (ws) {
        ws.onopen = null;
        ws.onmessage = null;
        ws.onerror = null;
        ws.onclose = null;
        if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
            ws.close();
        }
        ws = null;
    }

    ws = new WebSocket(wsUrl);

    ws.onopen = () => {
        console.log('WebSocket connected');
        ReconnectPolicy.reset();
        serverDisconnected = false;
        reconnecting = false;
        hideBanner();

        stopHeartbeatTimers();

        // Heartbeat ping
        lastPongTime = Date.now();
        heartbeatTimer = setInterval(() => {
            sendMsg('ping', { timestamp: Date.now() });
        }, CONFIG.heartbeat.INTERVAL);

        // Pong timeout checker
        pongCheckTimer = setInterval(() => {
            // Skip the check while the tab is hidden: timers are
            // throttled/frozen, so `elapsed` reflects wall-clock
            // silence (including the frozen stretch), not real
            // server silence. The foreground probe (visibilitychange
            // handler) does the real verification when we come back.
            //
            // The `document.hidden` check also covers the edge case
            // where the page was loaded in a background tab and
            // `visibilitychange` never fired, leaving `lastHiddenAt`
            // at its initial 0.
            if (document.hidden || lastHiddenAt > lastPongTime) return;
            const elapsed = Date.now() - lastPongTime;
            if (elapsed > CONFIG.heartbeat.PONG_TIMEOUT) {
                console.error('Server pong timeout, forcing reconnect');
                if (ws) ws.close();
            }
        }, CONFIG.heartbeat.PONG_CHECK_INTERVAL);

        const authPayload = Auth.wsPayload();
        if (authPayload) {
            sendMsg('auth', authPayload);
        } else {
            onAuthenticated();
        }
    };

    ws.onmessage = (event) => {
        try {
            const msg = JSON.parse(event.data);

            switch (msg.type) {
                case 'auth_ok':
                    console.log('Auth OK');
                    onAuthenticated();
                    break;
                case 'auth_fail':
                    console.error('Auth failed:', msg.data.reason);
                    authFailed = true;
                    ReconnectPolicy.count = CONFIG.reconnect.MAX_RETRIES;
                    showLoginError(msg.data.reason);
                    if (ws) ws.close();
                    break;
                case 'ready':
                    console.log('Terminal ready:', msg.data);
                    setStatus(true);
                    hideBanner();
                    updateSessionInfo(msg.data.session_id);
                    readOnly = msg.data.readonly || false;
                    setReadOnlyBadge(readOnly);
                    if (readOnly) {
                        writeSystemMessage('Read-only mode: your keystrokes will not be sent.');
                    }
                    break;
                case 'output':
                    term.write(msg.data.payload);
                    break;
                case 'pong':
                    lastPongTime = Date.now();
                    probeAwaitingPong = false;
                    break;
                case 'disconnect':
                    console.log('Disconnected:', msg.data.reason);
                    writeSystemMessage(`Disconnected: ${msg.data.reason}`);
                    writeSystemMessage('Press Enter or refresh page to reconnect.');
                    setStatus(false);
                    setReadOnlyBadge(false);
                    clearSessionInfo();
                    serverDisconnected = true;
                    stopHeartbeatTimers();
                    showBanner('lost', 'Session ended. Press Enter or reconnect to start a new one.');
                    if (ws) ws.close();
                    break;
                case 'error':
                    console.error('Error:', msg.data);
                    if (msg.data.code === 'FILE_LIST_ERROR' && isFilePanelOpen()) {
                        renderFileListError(msg.data.message);
                    } else {
                        writeErrorMessage(`Error: ${msg.data.message}`);
                    }
                    if (msg.data.fatal) {
                        showToast(`Fatal error: ${msg.data.message}`, 'error');
                        serverDisconnected = true;
                        if (ws) ws.close();
                    }
                    break;
                case 'file_list_result':
                    renderFileList(msg.data);
                    break;
                default:
                    console.warn('Unknown message type:', msg.type);
                    break;
            }
        } catch (e) {
            console.error('Failed to parse message:', e);
        }
    };

    ws.onerror = (error) => {
        console.error('WebSocket error:', error);
        // The subsequent onclose handler surfaces the reconnect banner;
        // only toast when the user is already in an active session, so
        // initial connection failures don't double-notify.
        if (hasAuthenticated && !serverDisconnected) {
            showToast('WebSocket 连接发生错误', 'error');
        }
    };

    ws.onclose = () => {
        console.log('WebSocket closed');
        setMenuVisible(false);
        stopHeartbeatTimers();

        // Auth failed: login overlay is visible, don't write to the
        // terminal canvas behind it (causes a visible flash).
        if (authFailed) {
            return;
        }

        // Auth is required but we haven't authenticated yet (e.g. the
        // login attempt hit a network failure). Keep the login overlay
        // as the primary surface so the user can retry, instead of
        // stacking a reconnect banner behind it.
        if (Auth.required && !hasAuthenticated) {
            setStatus(false);
            showLoginError('无法连接服务器，请检查网络后重试');
            return;
        }

        setStatus(false);

        if (serverDisconnected || ReconnectPolicy.exhausted) {
            if (serverDisconnected) {
                showBanner('lost', 'Connection closed by the server. Reconnect to start a new session.');
            } else {
                showBanner('lost', 'Connection lost. Check your network and try again.');
            }
            return;
        }

        const delay = ReconnectPolicy.nextDelay();
        statusText.textContent = `Reconnecting (${ReconnectPolicy.count}/${CONFIG.reconnect.MAX_RETRIES})...`;
        console.log(`Reconnecting in ${delay}ms (attempt ${ReconnectPolicy.count}/${CONFIG.reconnect.MAX_RETRIES})`);
        showBanner('reconnecting', `Reconnecting (${ReconnectPolicy.count}/${CONFIG.reconnect.MAX_RETRIES})...`);
        reconnectTimer = setTimeout(connect, delay);
    };
}

// When a backgrounded tab becomes visible again, don't trust the
// socket's reported state at face value:
//  - Desktop browsers throttle/freeze setInterval while hidden, so
//    heartbeatTimer/pongCheckTimer may not have run in a long time;
//    lastPongTime can be stale even though the connection is fine.
//  - iOS Safari (and other mobile browsers) can silently kill the
//    underlying socket while backgrounded without ever firing
//    onclose/onerror, so readyState can still read OPEN on a dead
//    connection.
// Either way, the fix is the same: record when the tab goes hidden
// (so pongCheckTimer can ignore stale elapsed time - see its guard
// above) and, on return to visibility, actively probe the
// connection with a short, fresh one-off timer instead of waiting
// on the (possibly frozen) interval timers.
document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
        lastHiddenAt = Date.now();
        return;
    }

    // Tab just became visible.
    if (visibilityProbeTimer !== null) {
        clearTimeout(visibilityProbeTimer);
        visibilityProbeTimer = null;
    }

    if (!ws || ws.readyState === WebSocket.CLOSED || ws.readyState === WebSocket.CLOSING) {
        // Already known to be down. Reconnect right away instead of
        // waiting out the backoff timer, since the user is looking
        // at the page now.
        if (!serverDisconnected && !authFailed && (!Auth.required || hasAuthenticated)) {
            if (reconnectTimer !== null) {
                clearTimeout(reconnectTimer);
                reconnectTimer = null;
            }
            ReconnectPolicy.reset();
            connect();
        }
        return;
    }

    if (ws.readyState !== WebSocket.OPEN) return; // still connecting; leave it be

    probeAwaitingPong = true;
    sendMsg('ping', { timestamp: Date.now() });

    visibilityProbeTimer = setTimeout(() => {
        visibilityProbeTimer = null;
        if (probeAwaitingPong) {
            probeAwaitingPong = false;
            console.warn('Foreground probe got no response, forcing reconnect');
            if (ws) ws.close();
        }
    }, CONFIG.heartbeat.FOREGROUND_PROBE_TIMEOUT);
});

/** Clear the heartbeat interval + pong checker (not the visibility probe). */
function stopHeartbeatTimers() {
    if (heartbeatTimer !== null) {
        clearInterval(heartbeatTimer);
        heartbeatTimer = null;
    }
    if (pongCheckTimer !== null) {
        clearInterval(pongCheckTimer);
        pongCheckTimer = null;
    }
}

/** Clear the one-off visibility probe (kept separate from the heartbeat). */
function stopVisibilityProbe() {
    if (visibilityProbeTimer !== null) {
        clearTimeout(visibilityProbeTimer);
        visibilityProbeTimer = null;
    }
    probeAwaitingPong = false;
}

// Manual reconnect: reset retry/backoff and the session-ended flag,
// then start a fresh connection (rejoin the current session if it is
// still alive on the server, otherwise the server creates a new one).
btnReconnect.addEventListener('click', () => {
    ReconnectPolicy.reset();
    serverDisconnected = false;
    writeSystemMessage('Reconnecting...');
    showBanner('reconnecting', 'Reconnecting...');
    connect();
});

// Called after auth succeeds (or if no auth needed)
function onAuthenticated() {
    hasAuthenticated = true;
    readOnly = false;
    setReadOnlyBadge(false);
    setMenuVisible(true);
    loginOverlay.classList.add('hidden');
    loginSubmit.disabled = false;
    loginSubmit.textContent = 'Login';

    // Send resize first (required by server), then optionally join
    sendResize();

    const rejoinId = currentSessionId || joinSessionId;
    if (rejoinId) {
        sendMsg('join', { session_id: rejoinId });
    }
}

// ============================================================
// Login form handling
// ============================================================
const loginOverlay = document.getElementById('login-overlay');
const loginUsername = document.getElementById('login-username');
const loginPassword = document.getElementById('login-password');
const loginToken = document.getElementById('login-token');
const loginSubmit = document.getElementById('login-submit');
const loginError = document.getElementById('login-error');

/** @param {string} msg */
function showLoginError(msg) {
    loginError.textContent = msg;
    loginError.style.display = 'block';
    loginOverlay.classList.remove('hidden');
    loginSubmit.disabled = false;
    loginSubmit.textContent = 'Login';
}

loginSubmit.addEventListener('click', () => {
    loginError.style.display = 'none';
    if (Auth.required) {
        if (!Auth.collectFromForm()) {
            showLoginError(Auth.isToken() ? 'Please enter a token' : 'Please enter username and password');
            return;
        }
        loginSubmit.disabled = true;
        loginSubmit.textContent = 'Connecting...';
        ReconnectPolicy.reset();
        connect();
    }
});

// Allow Enter key to submit
[loginUsername, loginPassword, loginToken].forEach(el => {
    el.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') loginSubmit.click();
    });
});

// ============================================================
// Startup: check auth config, then connect
// ============================================================
async function checkAuth() {
    try {
        const resp = await fetch('/api/config');
        const config = await resp.json();
        setServerConfig(config);
        const authMethod = Auth.init(config);
        if (authMethod) {
            loginOverlay.classList.remove('hidden');
            statusIndicator.classList.remove('connected', 'disconnected', 'reconnecting');
            statusIndicator.classList.add('pending');
            statusText.textContent = 'Login required';
            hideBanner();
            setTimeout(() => {
                (Auth.isToken() ? loginToken : loginUsername).focus();
            }, 0);
        } else {
            connect();
        }
    } catch (e) {
        console.error('Failed to check auth config:', e);
        showToast('无法获取服务器配置，正在尝试直接连接…', 'error');
        connect();
    }
}

// ============================================================
// Terminal input / resize
// ============================================================

// Send user input to server (or reconnect on Enter after disconnect).
// The reconnect path is debounced: while a reconnect is already in
// flight, further Enter presses are ignored.
term.onData((data) => {
    if (serverDisconnected) {
        if ((data === '\r' || data === '\n') && !reconnecting) {
            reconnecting = true;
            writeSystemMessage('Reconnecting...');
            connect();
        }
        return;
    }
    if (!readOnly) {
        sendMsg('input', { payload: data });
    }
});

initMobileKeys(
    () => serverDisconnected || readOnly,
    (seq) => sendMsg('input', { payload: seq }),
);

// Font-size changes (and fullscreen refits) must re-send the resize.
onFontSizeChange(sendResize);

// Handle terminal resize with debounce
let resizeTimer = null;
window.addEventListener('resize', () => {
    fitAddon.fit();
    if (resizeTimer !== null) {
        clearTimeout(resizeTimer);
    }
    resizeTimer = setTimeout(() => {
        resizeTimer = null;
        sendResize();
    }, CONFIG.ui.RESIZE_DEBOUNCE);
});

// ============================================================
// Dropdown menus (settings + files)
// ============================================================
const menuWrapper = document.getElementById('menu-wrapper');
const btnMenu = document.getElementById('btn-menu');
const menuDropdown = document.getElementById('menu-dropdown');
const settingsWrapper = document.getElementById('settings-wrapper');
const btnSettings = document.getElementById('btn-settings');
const settingsMenu = document.getElementById('settings-menu');
const uploadPanel = document.getElementById('upload-panel');

function closeAllMenus() {
    menuDropdown.classList.remove('open');
    settingsMenu.classList.remove('open');
    uploadPanel.classList.remove('open');
}

btnMenu.addEventListener('click', (e) => {
    e.stopPropagation();
    const willOpen = !menuDropdown.classList.contains('open');
    closeAllMenus();
    if (willOpen) menuDropdown.classList.add('open');
});

btnSettings.addEventListener('click', (e) => {
    e.stopPropagation();
    const willOpen = !settingsMenu.classList.contains('open');
    closeAllMenus();
    if (willOpen) settingsMenu.classList.add('open');
});

// Close menus on any outside click
document.addEventListener('click', (e) => {
    if (!e.target.closest('#menu-wrapper') && !e.target.closest('#settings-wrapper') && !e.target.closest('#upload-indicator')) {
        closeAllMenus();
    }
});

// Menu item clicks close the menu
menuDropdown.addEventListener('click', (e) => {
    if (e.target.closest('.menu-item')) closeAllMenus();
});
settingsMenu.addEventListener('click', (e) => {
    if (e.target.closest('.menu-item')) closeAllMenus();
});

const btnClear = document.getElementById('btn-clear');
btnClear.addEventListener('click', () => {
    term.clear();
    closeAllMenus();
});

const btnDownload = document.getElementById('btn-download');
btnDownload.addEventListener('click', () => {
    openFilePanel();
});

function setMenuVisible(visible) {
    if (visible) {
        menuWrapper.classList.remove('hidden');
    } else {
        menuWrapper.classList.add('hidden');
        menuDropdown.classList.remove('open');
    }
}

// ============================================================
// Escape-key handling (menus + modals, without stealing terminal Esc)
// ============================================================
document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    if (menuDropdown.classList.contains('open') || settingsMenu.classList.contains('open') || uploadPanel.classList.contains('open')) {
        closeAllMenus();
    } else if (isConfirmOpen()) {
        cancelConfirm();
    } else if (isFilePanelOpen()) {
        hideFilePanel();
    }
    // Otherwise, let xterm handle Esc (send to terminal).
});

// ============================================================
// Wire the file panel and transfer modules to the WS layer
// ============================================================
initTransfer(
    () => currentSessionId,
    closeAllMenus,
);

initFilePanel((path) => {
    if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: 'file_list', data: { path, show_hidden: showHiddenEnabled() } }));
    } else {
        cancelLoading();
        renderNotConnected();
    }
});

// ============================================================
// Start
// ============================================================
checkAuth();