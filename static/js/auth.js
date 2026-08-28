// ============================================================
// auth.js — authentication state and credential handling.
// Aggregates logic that was previously scattered across
// checkAuth(), ws.onopen, and getAuthHeader().
// ============================================================

/** UTF-8-safe Base64 encoding (btoa only supports Latin-1).
 *  @param {string} str
 *  @returns {string}
 */
function b64Encode(str) {
    const bytes = new TextEncoder().encode(str);
    let binary = '';
    for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
    return btoa(binary);
}

/**
 * Auth module. Call `init(config)` once with the /api/config payload
 * before connecting.
 */
export const Auth = {
    /** Whether the server requires WS authentication. */
    required: false,
    /** @type {{ method: string, credentials: string } | null} */
    _credentials: null,

    /** Whether the configured auth method is token-based. */
    isToken() {
        return !!this._credentials && this._credentials.method === 'token';
    },

    /**
     * Initialize from the /api/config response. Shows/hides the
     * appropriate login fields and wires the token input listener.
     * @param {object} config
     * @returns {{ method: 'basic'|'token' } | null} the auth method, if required
     */
    init(config) {
        if (!config.auth_method) {
            this.required = false;
            this._credentials = null;
            return null;
        }
        this.required = true;
        const basicFields = document.getElementById('basic-auth-fields');
        const tokenFields = document.getElementById('token-auth-fields');
        const loginToken = document.getElementById('login-token');

        if (config.auth_method === 'token') {
            basicFields.style.display = 'none';
            tokenFields.style.display = 'block';
            this._credentials = { method: 'token', credentials: '' };
            loginToken.addEventListener('input', () => {
                this._credentials.credentials = loginToken.value;
            });
        } else {
            this._credentials = { method: 'basic', credentials: '' };
        }
        return { method: config.auth_method };
    },

    /** Whether the user has entered (or previously had) credentials. */
    hasCredentials() {
        return !!this._credentials && !!this._credentials.credentials;
    },

    /**
     * Read the login form into the credential store. Returns false when
     * required fields are empty (the caller shows the error).
     * @returns {boolean}
     */
    collectFromForm() {
        if (!this._credentials) return false;
        if (this._credentials.method === 'basic') {
            const username = document.getElementById('login-username').value;
            const password = document.getElementById('login-password').value;
            if (!username || !password) return false;
            this._credentials.credentials = b64Encode(username + ':' + password);
        } else if (this._credentials.method === 'token') {
            if (!this._credentials.credentials) return false;
        }
        return true;
    },

    /** Payload for the WS `auth` message, or null when not required. */
    wsPayload() {
        if (!this.required || !this._credentials) return null;
        return { method: this._credentials.method, credentials: this._credentials.credentials };
    },

    /** Value for the HTTP `Authorization` header, or null. */
    httpHeader() {
        if (!this.required || !this._credentials || !this._credentials.credentials) return null;
        if (this._credentials.method === 'basic') return 'Basic ' + this._credentials.credentials;
        if (this._credentials.method === 'token') return 'Bearer ' + this._credentials.credentials;
        return null;
    },
};
