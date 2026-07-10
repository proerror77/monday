use alpha_engine::llm::{LlmConfig, OpenAiCompatibleClient};

#[test]
#[ignore = "requires ALPHA_LLM_ENDPOINT, ALPHA_LLM_API_KEY, and ALPHA_LLM_MODEL"]
fn real_llm_call_writes_hypothesis_artifact() {
    let client = OpenAiCompatibleClient::new(LlmConfig::from_env().unwrap()).unwrap();
    let artifact = client
        .generate_hypothesis(
            "Propose one bounded, testable factor hypothesis for BTCUSDT using field oi_delta_5m and an allowed operator.",
        )
        .unwrap();
    assert!(!artifact.hypothesis.trim().is_empty());
    assert!(!artifact.prompt_hash.trim().is_empty());
    assert!(artifact.token_usage.total_tokens > 0);
    let path = std::env::temp_dir().join("alpha-engine-real-llm-hypothesis.json");
    std::fs::write(path, serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();
}
