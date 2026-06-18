/// Server module - HTTP and WebSocket server implementation
pub mod api;
mod http;
pub mod websocket;

pub use http::start_server;
