fn main() {
    eprintln!("{}", ploy::compatibility_notice());
    eprintln!(
        "Available workspace apps: {}",
        ploy::workspace_apps().join(", ")
    );
}
