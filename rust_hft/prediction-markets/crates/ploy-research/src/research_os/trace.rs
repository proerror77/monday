use sha2::{Digest, Sha256};

pub fn trace_hash(
    hash_prev: Option<&str>,
    run_id: &str,
    event_type: &str,
    agent_name: &str,
    input_json: &serde_json::Value,
    output_json: &serde_json::Value,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(hash_prev.unwrap_or("").as_bytes());
    hasher.update(b"\n");
    hasher.update(run_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(event_type.as_bytes());
    hasher.update(b"\n");
    hasher.update(agent_name.as_bytes());
    hasher.update(b"\n");
    hasher.update(input_json.to_string().as_bytes());
    hasher.update(b"\n");
    hasher.update(output_json.to_string().as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_hash_changes_when_output_changes() {
        let input = serde_json::json!({"a": 1});
        let out_a = serde_json::json!({"candidate": "a"});
        let out_b = serde_json::json!({"candidate": "b"});
        let hash_a = trace_hash(
            None,
            "run-1",
            "generate",
            "research_manager",
            &input,
            &out_a,
        );
        let hash_b = trace_hash(
            None,
            "run-1",
            "generate",
            "research_manager",
            &input,
            &out_b,
        );
        assert_ne!(hash_a, hash_b);
    }
}
