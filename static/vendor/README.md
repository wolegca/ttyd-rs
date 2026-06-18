# Vendor Libraries

This directory contains third-party JavaScript libraries that are embedded into the application for security and reliability.

## Why Local Hosting?

All dependencies are hosted locally instead of using CDN links to:

1. **Security**: Eliminate the risk of CDN hijacking or man-in-the-middle attacks
2. **Reliability**: No dependency on external services
3. **Privacy**: No third-party tracking or external requests
4. **Offline Support**: Works without internet connection
5. **Performance**: Faster load times (no DNS lookup, SSL handshake to CDN)

## Included Libraries

### xterm.js v5.3.0
- **Files**: `xterm.js`, `xterm.css`
- **Purpose**: Terminal emulator for the web
- **License**: MIT
- **Source**: https://github.com/xtermjs/xterm.js

### xterm-addon-fit v0.8.0
- **File**: `xterm-addon-fit.js`
- **Purpose**: Fit terminal to container dimensions
- **License**: MIT
- **Source**: https://github.com/xtermjs/xterm.js

### xterm-addon-web-links v0.9.0
- **File**: `xterm-addon-web-links.js`
- **Purpose**: Add clickable web links to terminal
- **License**: MIT
- **Source**: https://github.com/xtermjs/xterm.js

## Updating Libraries

To update these libraries to newer versions:

```bash
cd static/vendor

# Download new versions
curl -L -o xterm.css "https://cdn.jsdelivr.net/npm/xterm@VERSION/css/xterm.css"
curl -L -o xterm.js "https://cdn.jsdelivr.net/npm/xterm@VERSION/lib/xterm.js"
curl -L -o xterm-addon-fit.js "https://cdn.jsdelivr.net/npm/xterm-addon-fit@VERSION/lib/xterm-addon-fit.js"
curl -L -o xterm-addon-web-links.js "https://cdn.jsdelivr.net/npm/xterm-addon-web-links@VERSION/lib/xterm-addon-web-links.js"

# Test the application
cargo run
```

## Verification

Verify no external dependencies:

```bash
# Check HTML for CDN links
grep -i "cdn\|unpkg\|jsdelivr" static/index.html

# Should return empty (no matches)
```

## Build Process

These files are automatically embedded into the binary at compile time using `rust-embed`, so they are served directly from memory without filesystem access.
