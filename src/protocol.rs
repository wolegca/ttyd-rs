/// WebSocket protocol message types
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
#[serde(rename_all = "snake_case")]
pub enum Message {
    /// Client authentication
    Auth(AuthData),

    /// User input from client
    Input(InputData),

    /// Terminal resize request
    Resize(ResizeData),

    /// Ping for keepalive
    Ping(PingData),

    /// Authentication successful
    AuthOk(AuthOkData),

    /// Authentication failed
    AuthFail(AuthFailData),

    /// Terminal output to client
    Output(OutputData),

    /// Pong response
    Pong(PongData),

    /// Error message
    Error(ErrorData),

    /// Disconnect notification
    Disconnect(DisconnectData),

    /// Terminal ready
    Ready(ReadyData),

    /// Client requests to join an existing session
    Join(JoinData),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthData {
    pub method: String,
    pub credentials: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputData {
    pub payload: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeData {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingData {
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthOkData {
    pub client_id: String,
    pub readonly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthFailData {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputData {
    pub payload: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PongData {
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorData {
    pub code: String,
    pub message: String,
    pub fatal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisconnectData {
    pub reason: String,
    pub code: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyData {
    pub session_id: String,
    pub cols: u16,
    pub rows: u16,
    pub readonly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinData {
    pub session_id: String,
}

impl Message {
    /// Parse a message from JSON text
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// Serialize a message to JSON text
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_message_serialization() {
        let msg = Message::Input(InputData {
            payload: "ls -la".to_string(),
        });

        let json = msg.to_json().unwrap();
        assert!(json.contains("input"));
        assert!(json.contains("ls -la"));

        let parsed = Message::from_json(&json).unwrap();
        match parsed {
            Message::Input(data) => assert_eq!(data.payload, "ls -la"),
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_resize_message() {
        let msg = Message::Resize(ResizeData { cols: 80, rows: 24 });

        let json = msg.to_json().unwrap();
        let parsed = Message::from_json(&json).unwrap();

        match parsed {
            Message::Resize(data) => {
                assert_eq!(data.cols, 80);
                assert_eq!(data.rows, 24);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_all_message_types_roundtrip() {
        let messages = vec![
            Message::Auth(AuthData {
                method: "basic".to_string(),
                credentials: "dXNlcjpwYXNz".to_string(),
            }),
            Message::AuthOk(AuthOkData {
                client_id: "client-1".to_string(),
                readonly: false,
            }),
            Message::AuthFail(AuthFailData {
                reason: "bad creds".to_string(),
            }),
            Message::Output(OutputData {
                payload: "hello\n".to_string(),
            }),
            Message::Ping(PingData { timestamp: 12345 }),
            Message::Pong(PongData { timestamp: 12345 }),
            Message::Error(ErrorData {
                code: "ERR".to_string(),
                message: "something broke".to_string(),
                fatal: true,
            }),
            Message::Disconnect(DisconnectData {
                reason: "bye".to_string(),
                code: 0,
            }),
            Message::Ready(ReadyData {
                session_id: "sess-2".to_string(),
                cols: 80,
                rows: 24,
                readonly: false,
            }),
            Message::Join(JoinData {
                session_id: "sess-2".to_string(),
            }),
        ];

        for msg in messages {
            let json = msg.to_json().unwrap();
            let parsed = Message::from_json(&json).unwrap();
            // Re-serialize to verify lossless roundtrip
            let json2 = parsed.to_json().unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn test_invalid_json_returns_error() {
        let result = Message::from_json("not valid json");
        assert!(result.is_err());
    }

    #[test]
    fn test_unknown_message_type_returns_error() {
        let result = Message::from_json(r#"{"type":"unknown","data":{}}"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_type_field_returns_error() {
        let result = Message::from_json(r#"{"payload":"hello"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_auth_message_json_structure() {
        let msg = Message::Auth(AuthData {
            method: "token".to_string(),
            credentials: "abc123".to_string(),
        });
        let json = msg.to_json().unwrap();
        assert!(json.contains(r#""type":"auth""#));
        assert!(json.contains(r#""method":"token""#));
        assert!(json.contains(r#""credentials":"abc123""#));
    }

    #[test]
    fn test_ready_message_fields() {
        let msg = Message::Ready(ReadyData {
            session_id: "s1".to_string(),
            cols: 120,
            rows: 40,
            readonly: true,
        });
        let json = msg.to_json().unwrap();
        let parsed = Message::from_json(&json).unwrap();
        match parsed {
            Message::Ready(data) => {
                assert_eq!(data.session_id, "s1");
                assert_eq!(data.cols, 120);
                assert_eq!(data.rows, 40);
                assert!(data.readonly);
            }
            _ => panic!("Expected Ready message"),
        }
    }

    #[test]
    fn test_error_message_fatal_flag() {
        let msg = Message::Error(ErrorData {
            code: "FATAL_ERR".to_string(),
            message: "critical failure".to_string(),
            fatal: true,
        });
        let json = msg.to_json().unwrap();
        let parsed = Message::from_json(&json).unwrap();
        match parsed {
            Message::Error(data) => {
                assert!(data.fatal);
                assert_eq!(data.code, "FATAL_ERR");
            }
            _ => panic!("Expected Error message"),
        }
    }

    #[test]
    fn test_join_message_roundtrip() {
        let msg = Message::Join(JoinData {
            session_id: "abc-123".to_string(),
        });
        let json = msg.to_json().unwrap();
        assert!(json.contains(r#""type":"join""#));
        assert!(json.contains(r#""session_id":"abc-123""#));

        let parsed = Message::from_json(&json).unwrap();
        match parsed {
            Message::Join(data) => assert_eq!(data.session_id, "abc-123"),
            _ => panic!("Expected Join message"),
        }
    }
}
