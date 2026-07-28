const BUILD_SOURCE_REVISION: &str = match option_env!("MONDAY_SOURCE_REVISION") {
    Some(value) => value,
    None => "unbound-source-revision",
};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if matches!(args.get(1).map(String::as_str), Some("--version" | "-V")) {
        println!("new-ploy-runner {BUILD_SOURCE_REVISION}");
        return;
    }
    ploy_runner_host::run_with_implicit_run_args(args).await;
}
