#[tokio::main]
async fn main() {
    ploy_runner_host::run_mode_binary(std::env::args().collect()).await;
}
