use std::fmt;
use zeroize::ZeroizeOnDrop;

/// 秘密值 - 自動清理內存，禁止 Debug/Display 輸出
#[derive(Clone, ZeroizeOnDrop)]
pub struct SecretValue {
    #[zeroize(skip)]
    name: String,

    #[zeroize(drop)]
    data: Vec<u8>,
}

impl SecretValue {
    pub fn new(name: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            data,
        }
    }

    pub fn as_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.data)
    }

    pub fn to_string_safe(&self) -> Result<String, std::str::Utf8Error> {
        self.as_str().map(|s| s.to_string())
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretValue")
            .field("name", &self.name)
            .field("data", &"***REDACTED***")
            .finish()
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretValue({})", &self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_value_redaction() {
        let secret = SecretValue::new("api_key", b"super_secret".to_vec());
        let debug_str = format!("{:?}", secret);
        assert!(!debug_str.contains("super_secret"));
        assert!(debug_str.contains("REDACTED"));
    }
}
