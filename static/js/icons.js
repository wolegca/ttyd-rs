// ============================================================
// icons.js — reusable SVG icons for dynamically created content
// ============================================================

// Shared "X" close glyph (used by toasts, transfer rows, error icon).
const CLOSE_PATH = '<path d="M4 4l8 8M12 4l-8 8"/>';

export const ICONS = {
    dir: '<svg viewBox="0 0 16 16" width="15" height="15" fill="currentColor"><path d="M1.75 1h4.06L7.5 2.75h6.75A1.75 1.75 0 0 1 16 4.5v8.75A1.75 1.75 0 0 1 14.25 15H1.75A1.75 1.75 0 0 1 0 13.25V2.75A1.75 1.75 0 0 1 1.75 1Z"/></svg>',
    file: '<svg viewBox="0 0 16 16" width="15" height="15" fill="currentColor"><path d="M4 1.75C4 .784 4.784 0 5.75 0h4.586c.464 0 .909.184 1.237.513l2.914 2.914c.329.328.513.773.513 1.237v9.586A1.75 1.75 0 0 1 13.25 16h-7.5A1.75 1.75 0 0 1 4 14.25Zm2.5-.25v3h3.5V1.5Z"/></svg>',
    up: '<svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M9.5 3.5L4.5 8.5M4.5 8.5h5v5"/></svg>',
    close: `<svg viewBox="0 0 16 16" width="11" height="11" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round">${CLOSE_PATH}</svg>`,
    toastSuccess: '<svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M3 8.5l3.5 3.5L13 4.5"/></svg>',
    toastError: `<svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round">${CLOSE_PATH}</svg>`,
    toastInfo: '<svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"><circle cx="8" cy="8" r="6.5"/><path d="M8 7.5V11M8 5.2v.1"/></svg>',
};
