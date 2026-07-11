//! gRPC 控制服務 Helper
//!
//! 提供遠程控制交易引擎的 gRPC 接口

use engine::ExecutionControlHandle;
use tokio::task::JoinHandle;
use tracing::info;

/// 啟動 gRPC 控制服務
pub fn spawn_grpc_server(execution_control: ExecutionControlHandle, port: u16) -> JoinHandle<()> {
    info!("啟動 gRPC 控制服務: 0.0.0.0:{}", port);
    hft_grpc_server::spawn_grpc_server(execution_control, port)
}
