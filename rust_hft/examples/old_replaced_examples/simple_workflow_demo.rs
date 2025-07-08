/*!
 * 簡化工作流演示 - 展示重構架構威力
 * 
 * 3個步驟展示完整機器學習流程：
 * 數據收集 → 模型訓練 → 評估部署
 */

use rust_hft::{WorkflowExecutor, WorkflowStep, StepResult, CommonArgs, TrainingArgs};
use anyhow::Result;
use async_trait::async_trait;
use clap::Parser;
use tracing::info;
use std::time::Duration;

/// 數據收集步驟
struct DataStep {
    symbol: String,
}

#[async_trait]
impl WorkflowStep for DataStep {
    fn name(&self) -> &str { "Data Collection" }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("📊 Collecting data for {}", self.symbol);
        tokio::time::sleep(Duration::from_secs(2)).await;
        Ok(StepResult::success("Data collected: 10,000 samples")
           .with_metric("samples", 10000.0))
    }
}

/// 訓練步驟
struct TrainingStep {
    epochs: usize,
}

#[async_trait]
impl WorkflowStep for TrainingStep {
    fn name(&self) -> &str { "Model Training" }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("🧠 Training model for {} epochs", self.epochs);
        tokio::time::sleep(Duration::from_secs(3)).await;
        Ok(StepResult::success("Training completed")
           .with_metric("final_loss", 0.05))
    }
}

/// 評估步驟
struct EvalStep;

#[async_trait]
impl WorkflowStep for EvalStep {
    fn name(&self) -> &str { "Model Evaluation" }
    
    async fn execute(&mut self) -> Result<StepResult> {
        info!("📈 Evaluating model performance");
        tokio::time::sleep(Duration::from_secs(1)).await;
        Ok(StepResult::success("Model ready for deployment")
           .with_metric("accuracy", 0.72))
    }
}

#[derive(Parser)]
struct Args {
    #[command(flatten)]
    common: CommonArgs,
    
    #[command(flatten)]  
    training: TrainingArgs,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
        
    let args = Args::parse();
    
    info!("🚀 Starting ML Workflow for {}", args.common.symbol);
    
    // 創建工作流 - 僅需3行！
    let mut workflow = WorkflowExecutor::new()
        .add_step(Box::new(DataStep { symbol: args.common.symbol.clone() }))
        .add_step(Box::new(TrainingStep { epochs: args.training.epochs }))
        .add_step(Box::new(EvalStep));
    
    // 執行工作流
    let report = workflow.execute().await?;
    report.print_detailed_report();
    
    if report.success {
        info!("🎉 Workflow completed successfully!");
    } else {
        info!("⚠️  Workflow completed with issues");
    }
    
    Ok(())
}