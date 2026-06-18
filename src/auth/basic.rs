/// Basic authentication implementation
use base64::{Engine as _, engine::general_purpose};

pub struct BasicAuth {
    username: String,
    password: String,
}

impl BasicAuth {
    pub fn new(username: String, password: String) -> Self {
        Self { username, password }
    }

    /// Validate credentials encoded as "username:password" in base64
    pub fn validate(&self, credentials: &str) -> bool {
        match general_purpose::STANDARD.decode(credentials) {
            Ok(decoded) => match String::from_utf8(decoded) {
                Ok(decoded_str) => {
                    let parts: Vec<&str> = decoded_str.splitn(2, ':').collect();
                    if parts.len() == 2 {
                        parts[0] == self.username && parts[1] == self.password
                    } else {
                        false
                    }
                }
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    /// Extract credentials from Authorization header
    #[allow(dead_code)]
    pub fn extract_from_header(header: &str) -> Option<String> {
        header
            .strip_prefix("Basic ")
            .map(|credentials| credentials.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_auth_valid() {
        let auth = BasicAuth::new("admin".to_string(), "secret".to_string());

        // "admin:secret" in base64
        let credentials = general_purpose::STANDARD.encode("admin:secret");
        assert!(auth.validate(&credentials));
    }

    #[test]
    fn test_basic_auth_invalid() {
        let auth = BasicAuth::new("admin".to_string(), "secret".to_string());

        // Wrong password
        let credentials = general_purpose::STANDARD.encode("admin:wrong");
        assert!(!auth.validate(&credentials));
    }

    #[test]
    fn test_extract_from_header() {
        let header = "Basic YWRtaW46c2VjcmV0";
        let result = BasicAuth::extract_from_header(header);
        assert_eq!(result, Some("YWRtaW46c2VjcmV0".to_string()));

        let invalid_header = "Bearer token";
        let result = BasicAuth::extract_from_header(invalid_header);
        assert_eq!(result, None);
    }
}
