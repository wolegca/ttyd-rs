// ============================================================
// terminal.js — xterm.js instance, font/cursor settings, copy,
// fullscreen, and the mobile special-key toolbar.
// ============================================================

import { CONFIG, Prefs } from './config.js';
import { showToast } from './toast.js';

const { DEFAULT_FONT_SIZE, MIN_FONT_SIZE, MAX_FONT_SIZE } = CONFIG.ui;

// Persisted user preferences (clamped to the valid range; handles
// corrupt localStorage values).
export const fontSize = {
    value: Prefs.getInt(CONFIG.storage.KEYS.FONT_SIZE, DEFAULT_FONT_SIZE, MIN_FONT_SIZE, MAX_FONT_SIZE),
};

export const cursorBlink = Prefs.getBool(CONFIG.storage.KEYS.CURSOR_BLINK, true);

export const term = new Terminal({
    cursorBlink: cursorBlink,
    fontSize: fontSize.value,
    fontFamily: '"0xProto Nerd Font Mono", Menlo, Monaco, "Consolas", "DejaVu Sans Mono", "Courier New", monospace',
    letterSpacing: 0,
    theme: {
        background: '#1e1e1e',
        foreground: '#d4d4d4',
        cursor: '#aeafad',
        cursorAccent: '#1e1e1e',
        selectionBackground: 'rgba(78, 201, 176, 0.28)',
        black: '#000000',
        red: '#cd3131',
        green: '#0dbc79',
        yellow: '#e5e510',
        blue: '#2472c8',
        magenta: '#bc3fbc',
        cyan: '#11a8cd',
        white: '#e5e5e5',
        brightBlack: '#666666',
        brightRed: '#f14c4c',
        brightGreen: '#23d18b',
        brightYellow: '#f5f543',
        brightBlue: '#3b8eea',
        brightMagenta: '#d670d6',
        brightCyan: '#29b8db',
        brightWhite: '#e5e5e5',
    },
});

export const fitAddon = new FitAddon.FitAddon();
term.loadAddon(fitAddon);

const webLinksAddon = new WebLinksAddon.WebLinksAddon();
term.loadAddon(webLinksAddon);

term.open(document.getElementById('terminal'));
fitAddon.fit();

// ============================================================
// Font size controls (persisted)
// ============================================================
const fontSizeLabel = document.getElementById('font-size-label');

function updateFontSizeLabel() {
    fontSizeLabel.textContent = String(fontSize.value);
}
updateFontSizeLabel();

/** @param {(size: number) => void} notify */
export function onFontSizeChange(notify) {
    applyFontSizeHook = notify;
}
let applyFontSizeHook = () => {};
function applyFontSize() {
    term.options.fontSize = fontSize.value;
    fitAddon.fit();
    applyFontSizeHook();
    updateFontSizeLabel();
    Prefs.set(CONFIG.storage.KEYS.FONT_SIZE, String(fontSize.value));
}

function zoomIn() {
    if (fontSize.value < MAX_FONT_SIZE) { fontSize.value++; applyFontSize(); }
}
function zoomOut() {
    if (fontSize.value > MIN_FONT_SIZE) { fontSize.value--; applyFontSize(); }
}
function resetFont() {
    if (fontSize.value !== DEFAULT_FONT_SIZE) { fontSize.value = DEFAULT_FONT_SIZE; applyFontSize(); }
}

document.getElementById('btn-font-inc').addEventListener('click', zoomIn);
document.getElementById('btn-font-dec').addEventListener('click', zoomOut);
document.getElementById('btn-font-reset').addEventListener('click', resetFont);

// Keyboard zoom (Ctrl/Cmd + = / - / 0)
window.addEventListener('keydown', (e) => {
    const mod = e.ctrlKey || e.metaKey;
    if (!mod) return;
    if (e.key === '=' || e.key === '+') { e.preventDefault(); zoomIn(); }
    else if (e.key === '-' || e.key === '_') { e.preventDefault(); zoomOut(); }
    else if (e.key === '0') { e.preventDefault(); resetFont(); }
});

// ============================================================
// Cursor blink toggle (persisted)
// ============================================================
const cursorBlinkToggle = document.getElementById('cursor-blink-toggle');
cursorBlinkToggle.checked = cursorBlink;
cursorBlinkToggle.addEventListener('change', () => {
    term.options.cursorBlink = cursorBlinkToggle.checked;
    Prefs.set(CONFIG.storage.KEYS.CURSOR_BLINK, String(cursorBlinkToggle.checked));
});

// ============================================================
// Fullscreen
// ============================================================
const btnFullscreen = document.getElementById('btn-fullscreen');
const FS_ENTER_SVG = '<svg viewBox="0 0 16 16" width="17" height="17" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M6 2.5H2.5V6M10 2.5h3.5V6M10 13.5h3.5V10M6 13.5H2.5V10"/></svg>';
const FS_EXIT_SVG = '<svg viewBox="0 0 16 16" width="17" height="17" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M6 2.5V6H2.5M10 2.5V6h3.5M13.5 10h-3.5V13.5M2.5 10h3.5v3.5"/></svg>';

function setFullscreenIcon(isFullscreen) {
    btnFullscreen.innerHTML = isFullscreen ? FS_EXIT_SVG : FS_ENTER_SVG;
    btnFullscreen.title = isFullscreen ? 'Exit fullscreen' : 'Toggle fullscreen';
}

function toggleFullscreen() {
    if (!document.fullscreenElement) {
        const p = document.documentElement.requestFullscreen();
        if (p && p.catch) p.catch(() => {});
    } else {
        const p = document.exitFullscreen();
        if (p && p.catch) p.catch(() => {});
    }
}
btnFullscreen.addEventListener('click', toggleFullscreen);
document.addEventListener('fullscreenchange', () => {
    setFullscreenIcon(!!document.fullscreenElement);
    setTimeout(() => { fitAddon.fit(); applyFontSizeHook(); }, CONFIG.ui.FULLSCREEN_FIT_DELAY);
});

// ============================================================
// Copy selection
// ============================================================
const copyBtn = document.getElementById('copy-selection');

term.onSelectionChange(() => {
    const hasSel = term.hasSelection();
    copyBtn.classList.toggle('visible', !!hasSel);
});
copyBtn.addEventListener('click', async () => {
    const sel = term.getSelection();
    if (!sel) return;
    try {
        await navigator.clipboard.writeText(sel);
        showToast('Copied to clipboard', 'success');
    } catch (e) {
        // Fallback for older browsers / non-secure contexts
        try {
            const ta = document.createElement('textarea');
            ta.value = sel;
            ta.style.position = 'fixed';
            ta.style.opacity = '0';
            document.body.appendChild(ta);
            ta.select();
            document.execCommand('copy');
            document.body.removeChild(ta);
            showToast('Copied to clipboard', 'success');
        } catch (_) {
            showToast('Copy failed', 'error');
        }
    }
});

// ============================================================
// Mobile special-key toolbar
// ============================================================
const MOBILE_KEY_SEQUENCES = {
    Escape: '\x1b',
    Tab: '\t',
    ArrowUp: '\x1b[A',
    ArrowDown: '\x1b[B',
    ArrowRight: '\x1b[C',
    ArrowLeft: '\x1b[D',
    CtrlC: '\x03',
    CtrlD: '\x04',
    CtrlL: '\x0c',
    PageUp: '\x1b[5~',
    PageDown: '\x1b[6~',
};

/** @param {() => boolean} inputBlocked returns true when input must not be sent */
export function initMobileKeys(inputBlocked, sendInput) {
    document.getElementById('mobile-keys').addEventListener('click', (e) => {
        const btn = e.target.closest('button[data-key]');
        if (!btn) return;
        if (inputBlocked()) return;
        const seq = MOBILE_KEY_SEQUENCES[btn.dataset.key];
        if (seq) sendInput(seq);
    });
}

// ============================================================
// System messages written into the terminal canvas
// ============================================================

/** Write an app-level system message to the terminal (dim gray).
 *  @param {string} msg
 */
export function writeSystemMessage(msg) {
    term.write(`\r\n\x1b[90m[${msg}]\x1b[0m\r\n`);
}

/** @param {string} msg */
export function writeErrorMessage(msg) {
    term.write(`\r\n\x1b[91m[${msg}]\x1b[0m\r\n`);
}
