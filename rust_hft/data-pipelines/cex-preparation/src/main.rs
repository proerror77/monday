#[tokio::main]
async fn main() -> anyhow::Result<()> {
    hft_cex_data_preparation::run().await
}
