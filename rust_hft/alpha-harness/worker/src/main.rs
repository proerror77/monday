#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    alpha_harness::worker_main().await
}
