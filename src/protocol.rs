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

    /// Client requests a directory listing
    FileList(FileListData),

    /// Server responds with directory listing
    FileListResult(FileListResultData),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileListData {
    /// Relative path to list (default: ".")
    #[serde(default = "default_file_list_path")]
    pub path: String,
    /// Whether to include hidden files
    #[serde(default)]
    pub show_hidden: bool,
}

fn default_file_list_path() -> String {
    ".".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileListResultData {
    pub path: String,
    pub entries: Vec<FileEntryData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntryData {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub modified: Option<String>,
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

    /// Serialize an `Output` message to JSON without going through serde.
    ///
    /// `Output` is by far the highest-volume message type (every burst of
    /// PTY bytes), so the fast path builds the JSON in a single allocation
    /// with no reflection and no intermediate `String` from
    /// `from_utf8_lossy().to_string()`.
    ///
    /// The result is byte-identical to `to_json` for the same payload:
    /// invalid UTF-8 is replaced with U+FFFD exactly like
    /// `String::from_utf8_lossy`, and escaping follows the serde_json rules
    /// (`\"`, `\\`, `\b`, `\f`, `\n`, `\r`, `\t`, `\u00XX`).
    pub fn output_json(payload: &[u8]) -> String {
        const PREFIX: &str = "{\"type\":\"output\",\"data\":{\"payload\":\"";
        const SUFFIX: &str = "\"}}";

        // Zero-copy for valid UTF-8; lossy (U+FFFD) replacement otherwise.
        let text = String::from_utf8_lossy(payload);
        let mut out = String::with_capacity(PREFIX.len() + text.len() + SUFFIX.len());
        out.push_str(PREFIX);
        Self::escape_json_payload(&mut out, &text);
        out.push_str(SUFFIX);
        out
    }

    /// Append `s` to `out` with serde_json-compatible string escaping.
    fn escape_json_payload(out: &mut String, s: &str) {
        const DIGITS: [u8; 16] = *b"0123456789abcdef";
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    let v = c as u32;
                    out.push_str("\\u");
                    out.push(DIGITS[(v >> 12) as usize] as char);
                    out.push(DIGITS[(v >> 8) as usize] as char);
                    out.push(DIGITS[(v >> 4) as usize] as char);
                    out.push(DIGITS[(v & 0xf) as usize] as char);
                }
                c => out.push(c),
            }
        }
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
    fn test_output_json_matches_serde() {
        let payloads: [&[u8]; 8] = [
            b"",
            b"hello world",
            b"line1\nline2\r\nend",
            b"quote \" and backslash \\ here",
            b"tab\there",
            b"ctl \x01 \x02 \x1f end",
            "unicode: 你好 🦀".as_bytes(),
            b"del \x7f char",
        ];
        for p in payloads {
            let expected = Message::Output(OutputData {
                payload: String::from_utf8_lossy(p).into_owned(),
            })
            .to_json()
            .unwrap();
            assert_eq!(Message::output_json(p), expected, "payload: {:?}", p);
        }
    }

    #[test]
    fn test_output_json_invalid_utf8_matches_lossy() {
        let raw = b"ok \xff\xfe bad";
        let expected = Message::Output(OutputData {
            payload: String::from_utf8_lossy(raw).into_owned(),
        })
        .to_json()
        .unwrap();
        assert_eq!(Message::output_json(raw), expected);
    }

    #[test]
    fn test_output_json_roundtrip() {
        let raw = "round \n trip \"quote\" \\ back 你好".as_bytes();
        let json = Message::output_json(raw);
        let parsed = Message::from_json(&json).unwrap();
        match parsed {
            Message::Output(d) => assert_eq!(d.payload, String::from_utf8_lossy(raw)),
            other => panic!("expected Output, got {other:?}"),
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

    #[test]
    fn test_file_list_message_roundtrip() {
        let msg = Message::FileList(FileListData {
            path: ".".to_string(),
            show_hidden: false,
        });
        let json = msg.to_json().unwrap();
        assert!(json.contains(r#""type":"file_list""#));

        let parsed = Message::from_json(&json).unwrap();
        match parsed {
            Message::FileList(data) => {
                assert_eq!(data.path, ".");
                assert!(!data.show_hidden);
            }
            _ => panic!("Expected FileList message"),
        }
    }

    #[test]
    fn test_file_list_result_message_roundtrip() {
        let msg = Message::FileListResult(FileListResultData {
            path: ".".to_string(),
            entries: vec![
                FileEntryData {
                    name: "src".to_string(),
                    size: 4096,
                    is_dir: true,
                    modified: Some("1700000000".to_string()),
                },
                FileEntryData {
                    name: "main.rs".to_string(),
                    size: 1234,
                    is_dir: false,
                    modified: None,
                },
            ],
        });
        let json = msg.to_json().unwrap();
        assert!(json.contains(r#""type":"file_list_result""#));

        let parsed = Message::from_json(&json).unwrap();
        match parsed {
            Message::FileListResult(data) => {
                assert_eq!(data.entries.len(), 2);
                assert!(data.entries[0].is_dir);
                assert!(!data.entries[1].is_dir);
            }
            _ => panic!("Expected FileListResult message"),
        }
    }

    #[test]
    fn test_file_list_default_path() {
        // When path is omitted, it should default to "."
        let json = r#"{"type":"file_list","data":{}}"#;
        let parsed = Message::from_json(json).unwrap();
        match parsed {
            Message::FileList(data) => {
                assert_eq!(data.path, ".");
                assert!(!data.show_hidden);
            }
            _ => panic!("Expected FileList message"),
        }
    }
}
