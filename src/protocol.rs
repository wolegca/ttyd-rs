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
    pub session_id: String,
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
}
