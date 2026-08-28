// ============================================================
// toast.js — toast notifications + styled confirm modal.
// ============================================================

import { CONFIG } from './config.js';
import { ICONS } from './icons.js';
import { Prefs } from './config.js';

// ============================================================
// Toast duration setting (persisted, seconds; 0 = never hide)
// ============================================================
const toastDurationInput = document.getElementById('toast-duration-input');

export const toast = {
    duration: Prefs.getInt(CONFIG.storage.KEYS.TOAST_DURATION, CONFIG.ui.TOAST_DEFAULT_DURATION, 0),
};

toastDurationInput.value = String(toast.duration);

function setToastDuration(value) {
    if (Number.isInteger(value) && value >= 0) {
        toast.duration = value;
        Prefs.set(CONFIG.storage.KEYS.TOAST_DURATION, String(value));
    }
    toastDurationInput.value = String(toast.duration);
}
toastDurationInput.addEventListener('change', () => {
    setToastDuration(parseInt(toastDurationInput.value, 10));
});
document.getElementById('btn-toast-dec').addEventListener('click', () => {
    setToastDuration(Math.max(0, toast.duration - 1));
});
document.getElementById('btn-toast-inc').addEventListener('click', () => {
    setToastDuration(toast.duration + 1);
});
document.getElementById('btn-toast-reset').addEventListener('click', () => {
    setToastDuration(CONFIG.ui.TOAST_DEFAULT_DURATION);
});

// ============================================================
// Toast notifications
// ============================================================
const toastContainer = document.getElementById('toast-container');
const TOAST_ICONS = { success: ICONS.toastSuccess, error: ICONS.toastError, info: ICONS.toastInfo };

/**
 * Show a toast notification.
 * @param {string} message
 * @param {'success'|'error'|'info'} [type='success']
 * @param {number} [duration] seconds; defaults to the configured value; 0 = sticky
 */
export function showToast(message, type = 'success', duration = toast.duration) {
    const el = document.createElement('div');
    el.className = `toast ${type}`;
    const icon = document.createElement('span');
    icon.className = 'toast-icon';
    icon.innerHTML = TOAST_ICONS[type] || TOAST_ICONS.info;
    const text = document.createElement('span');
    text.className = 'toast-text';
    text.textContent = message;
    const close = document.createElement('button');
    close.className = 'toast-close';
    close.setAttribute('aria-label', 'Dismiss notification');
    close.title = 'Dismiss';
    close.innerHTML = ICONS.close;
    close.addEventListener('click', () => dismiss());
    el.appendChild(icon);
    el.appendChild(text);
    el.appendChild(close);
    toastContainer.appendChild(el);

    let removed = false;
    let fadeTimer = null;
    function dismiss() {
        if (removed) return;
        removed = true;
        if (fadeTimer) clearTimeout(fadeTimer);
        el.classList.add('fade-out');
        setTimeout(() => el.remove(), CONFIG.ui.FADE_OUT_MS);
    }

    // duration === 0 means sticky: only dismissible manually
    if (duration > 0) {
        fadeTimer = setTimeout(dismiss, duration * 1000);
    }
}

// ============================================================
// Custom confirm modal (styled, replaces window.confirm)
// ============================================================
const confirmOverlay = document.getElementById('confirm-overlay');
const confirmMessage = document.getElementById('confirm-message');
const confirmOk = document.getElementById('confirm-ok');
const confirmCancel = document.getElementById('confirm-cancel');

/** Whether the confirm modal is currently visible. */
export function isConfirmOpen() {
    return !confirmOverlay.classList.contains('hidden');
}

/** Programmatically cancel the open confirm modal (Escape handler). */
export function cancelConfirm() {
    confirmCancel.click();
}

/**
 * Show a styled confirm dialog matching the app's theme.
 * @param {string} message
 * @param {string} [okLabel='Overwrite']
 * @returns {Promise<boolean>}
 */
export function showConfirm(message, okLabel = 'Overwrite') {
    confirmMessage.textContent = message;
    confirmOk.textContent = okLabel;
    confirmOverlay.classList.remove('hidden');
    requestAnimationFrame(() => confirmOk.focus());
    return new Promise((resolve) => {
        function cleanup(result) {
            confirmOverlay.classList.add('hidden');
            confirmOk.removeEventListener('click', onOk);
            confirmCancel.removeEventListener('click', onCancel);
            resolve(result);
        }
        function onOk() { cleanup(true); }
        function onCancel() { cleanup(false); }
        confirmOk.addEventListener('click', onOk);
        confirmCancel.addEventListener('click', onCancel);
    });
}
