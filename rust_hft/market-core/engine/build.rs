fn main() {
    // Allow the `loom` cfg for Loom concurrency testing framework
    println!("cargo::rustc-check-cfg=cfg(loom)");
}
