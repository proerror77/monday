use ploy_operator_contracts::schemas::{contract_schemas, serialize_schema};
use std::{env, fs, path::PathBuf};

fn schema_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("contracts")
        .join("schemas")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let check = env::args().any(|arg| arg == "--check");
    let schema_dir = schema_dir();
    fs::create_dir_all(&schema_dir)?;

    let mut stale = Vec::new();

    for contract in contract_schemas() {
        let path = schema_dir.join(contract.file_name);
        let json = serialize_schema(&(contract.schema)())?;

        if check {
            let current = fs::read_to_string(&path).unwrap_or_default();
            if current != json {
                stale.push(contract.file_name);
            }
        } else {
            fs::write(&path, json)?;
            println!("wrote {}", path.display());
        }
    }

    if !stale.is_empty() {
        return Err(format!("stale schema snapshots: {}", stale.join(", ")).into());
    }

    Ok(())
}
