use anyhow::{bail, Result};
use hft_market_pipeline::market_import::{normalize, read_json, NormalizeConfig};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--help" | "-h"] => println!("monday-market-pipeline normalize CONFIG.json\n  import CONFIG.json requires the import feature and explicitly admitted DB services"),
        ["normalize", path] => {
            let config: NormalizeConfig = read_json(std::path::Path::new(path))?;
            println!("{}", serde_json::to_string(&normalize(&config)?)?);
        }
        #[cfg(feature = "import")]
        ["import", path] => {
            let config: hft_market_pipeline::market_import::ImportConfig = read_json(std::path::Path::new(path))?;
            let result = tokio::runtime::Runtime::new()?.block_on(hft_market_pipeline::market_import::import(&config))?;
            println!("{}", serde_json::to_string(&result)?);
        }
        _ => bail!("usage: monday-market-pipeline normalize CONFIG.json | import CONFIG.json"),
    }
    Ok(())
}
