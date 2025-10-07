//! Minimal REST helpers placeholders.

#[derive(Debug, Clone)]
pub struct RestConfig {
    pub base_url: String,
}

impl Default for RestConfig {
    fn default() -> Self {
        Self { base_url: String::new() }
    }
}

