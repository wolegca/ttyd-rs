// ============================================================
// config.js — single source of truth for all frontend constants
// and the localStorage preference wrapper.
// ============================================================

export const CONFIG = {
    // Reconnect backoff. Exponential: BASE_DELAY * 2^(attempt-1), capped.
    reconnect: {
        MAX_RETRIES: 10,
        BASE_DELAY: 1000,
        MAX_DELAY: 30000,
    },

    // Heartbeat / liveness. NOTE: these are an implicit contract with the
    // server, which sends protocol-level WS pings every 30s and drops the
    // connection after 90s without a pong (see CLAUDE.md "Idle-connection
    // liveness"). Keep them in sync if the server changes.
    heartbeat: {
        INTERVAL: 30000,          // client ping interval
        PONG_TIMEOUT: 90000,      // force reconnect after this much silence
        PONG_CHECK_INTERVAL: 10000,
        FOREGROUND_PROBE_TIMEOUT: 5000, // fast verdict when tab becomes visible
    },

    ui: {
        DEFAULT_FONT_SIZE: 14,
        MIN_FONT_SIZE: 8,
        MAX_FONT_SIZE: 32,
        TOAST_DEFAULT_DURATION: 4,   // seconds; 0 = sticky
        UPLOAD_HIDE_DELAY: 4000,     // ms finished transfers stay visible
        RESIZE_DEBOUNCE: 100,        // ms
        LOADING_DELAY: 250,          // ms before file-panel spinner shows
        FULLSCREEN_FIT_DELAY: 120,   // ms before refit after fullscreen change
        FADE_OUT_MS: 300,            // toast fade-out duration
    },

    storage: {
        PREFIX: 'ttyd.',
        KEYS: {
            FONT_SIZE: 'fontSize',
            CURSOR_BLINK: 'cursorBlink',
            TOAST_DURATION: 'toastDuration',
            SHOW_HIDDEN: 'showHidden',
        },
    },
};

// ============================================================
// Prefs — namespaced localStorage wrapper.
// All persisted preferences go through here so keys and parsing
// live in exactly one place.
// ============================================================
export const Prefs = {
    _key(name) {
        return CONFIG.storage.PREFIX + name;
    },

    /** Read a raw string value, or `fallback` when missing.
     *  @param {string} name
     *  @param {string|null} [fallback=null]
     *  @returns {string|null}
     */
    get(name, fallback = null) {
        try {
            const v = localStorage.getItem(this._key(name));
            return v === null ? fallback : v;
        } catch {
            return fallback; // storage unavailable (private mode, etc.)
        }
    },

    /** Persist a raw string value. Failures are swallowed (best effort).
     *  @param {string} name
     *  @param {string} value
     */
    set(name, value) {
        try {
            localStorage.setItem(this._key(name), value);
        } catch {
            /* ignore: quota / privacy errors are non-fatal */
        }
    },

    /** Read an integer preference, clamped to [min, max], with fallback.
     *  @param {string} name
     *  @param {number} fallback
     *  @param {number} [min=-Infinity]
     *  @param {number} [max=Infinity]
     *  @returns {number}
     */
    getInt(name, fallback, min = -Infinity, max = Infinity) {
        const raw = parseInt(this.get(name, ''), 10);
        if (!Number.isInteger(raw)) return fallback;
        return Math.min(max, Math.max(min, raw));
    },

    /** Read a boolean preference ("true"/"false" strings).
     *  @param {string} name
     *  @param {boolean} fallback
     *  @returns {boolean}
     */
    getBool(name, fallback) {
        const v = this.get(name);
        if (v === null) return fallback;
        return v === 'true';
    },
};
