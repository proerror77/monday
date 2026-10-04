//! ACK-only shared data-plane controller. This command grants no experiment authority.
#[path = "research-data-service/service.rs"]
mod service;

fn main() -> anyhow::Result<()> {
    service::run()
}
