//! Pure immutable object identity and bounded name validation.
//! URL capabilities are reduced to object identity without performing IO.
use anyhow::{bail, Context};
use sha2::{Digest, Sha256};

pub const TOKYO_OSS_INTERNAL_ENDPOINT: &str = "oss-ap-northeast-1-internal.aliyuncs.com";

pub fn canonical_https_object(label: &str, value: &str) -> anyhow::Result<String> {
    if value != value.trim() || value.chars().any(char::is_control) {
        bail!("{label} URL must not contain surrounding whitespace or control characters");
    }
    if !value.starts_with("https://") {
        bail!("{label} URL must start with canonical https://");
    }
    let mut url = reqwest::Url::parse(value).with_context(|| format!("{label} URL is invalid"))?;
    if url.scheme() != "https" || url.host_str().is_none() {
        bail!("{label} URL must be HTTPS with a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("{label} URL must not contain userinfo credentials");
    }
    if url.fragment().is_some() || url.path() == "/" || url.path().ends_with('/') {
        bail!("{label} URL must identify one object without a fragment");
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

pub fn canonical_tokyo_oss_internal_object(label: &str, value: &str) -> anyhow::Result<String> {
    let canonical = canonical_https_object(label, value)?;
    let url = reqwest::Url::parse(&canonical).expect("canonical HTTPS object must parse");
    if url.port().is_some() {
        bail!("{label} URL must not override the OSS port");
    }
    let host = url
        .host_str()
        .context("canonical HTTPS object must retain a host")?;
    let bucket = if let Some(bucket) = host.strip_suffix(&format!(".{TOKYO_OSS_INTERNAL_ENDPOINT}"))
    {
        bucket
    } else {
        bail!("{label} URL must target the Tokyo OSS internal endpoint");
    };
    validate_dns_label("OSS bucket", bucket)?;
    Ok(canonical)
}

pub fn result_object_binds_attempt(object: &str, attempt_id: &str) -> anyhow::Result<bool> {
    let url = reqwest::Url::parse(object)?;
    Ok(url.path_segments().into_iter().flatten().any(|segment| {
        segment == attempt_id
            || segment.strip_prefix("attempt=") == Some(attempt_id)
            || segment.strip_suffix(".zip") == Some(attempt_id)
    }))
}

pub fn validate_identifier(label: &str, value: &str) -> anyhow::Result<()> {
    let value = value.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("{label} is invalid");
    }
    Ok(())
}

pub fn validate_dns_label(label: &str, value: &str) -> anyhow::Result<()> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 63
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
    {
        bail!("{label} must be a lowercase Kubernetes DNS label");
    }
    Ok(())
}

pub fn sha256_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_tokyo_oss_internal_object_keeps_virtual_host_and_strips_queries() {
        let canonical = canonical_tokyo_oss_internal_object(
            "result",
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/campaign-id=test/campaign-result.json?signature=x",
        )
        .unwrap();

        assert_eq!(
            canonical,
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/campaign-id=test/campaign-result.json"
        );
    }

    #[test]
    fn canonical_tokyo_oss_internal_object_rejects_non_internal_hosts() {
        for url in [
            "https://example.com/research/campaign-id=test/campaign-result.json",
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1.aliyuncs.com/research/campaign-id=test/campaign-result.json",
            "https://oss-ap-northeast-1-internal.aliyuncs.com/monday-lob-apne1-1045353359/research/campaign-id=test/campaign-result.json",
        ] {
            assert!(canonical_tokyo_oss_internal_object("result", url).is_err());
        }
    }
}
