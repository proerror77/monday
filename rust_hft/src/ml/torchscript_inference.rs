/*!
 * TorchScript 模型推理引擎
 * 
 * 只負責加載和執行 Python 訓練好的 TorchScript 模型
 * 不包含任何訓練邏輯
 */

#![cfg(feature = "torchscript")]

use anyhow::{Result, Context};
use tch::{CModule, Tensor, Device, Kind};
use std::path::Path;
use std::time::Instant;
use tracing::{info, debug, error};
use serde::{Serialize, Deserialize};

/// 推理結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceResult {
    /// 預測的動作（買入/賣出/持有）
    pub action: TradingAction,
    /// 建議的倉位大小（-1.0 到 1.0）
    pub position_size: f64,
    /// 預測的置信度
    pub confidence: f64,
    /// 推理延遲（微秒）
    pub latency_us: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TradingAction {
    Buy,
    Sell,
    Hold,
}

/// TorchScript 推理引擎
pub struct TorchScriptInference {
    /// 加載的模型
    model: CModule,
    /// 使用的設備（CPU/CUDA）
    device: Device,
    /// 模型路徑
    model_path: String,
    /// 推理統計
    stats: InferenceStats,
}

#[derive(Debug, Default)]
struct InferenceStats {
    total_inferences: u64,
    total_latency_us: u64,
    max_latency_us: u64,
    min_latency_us: u64,
}

impl TorchScriptInference {
    /// 從文件加載 TorchScript 模型
    pub fn load_model<P: AsRef<Path>>(model_path: P) -> Result<Self> {
        let start = Instant::now();
        let path_str = model_path.as_ref().display().to_string();
        
        // 自動選擇設備
        let device = if tch::Cuda::is_available() {
            info!("CUDA available, using GPU for inference");
            Device::Cuda(0)
        } else {
            info!("Using CPU for inference");
            Device::Cpu
        };
        
        // 加載 TorchScript 模型
        let model = CModule::load_on_device(&model_path, device)
            .with_context(|| format!("Failed to load TorchScript model from {}", path_str))?;
        
        let load_time = start.elapsed();
        info!("TorchScript model loaded from {} in {:?}", path_str, load_time);
        
        Ok(Self {
            model,
            device,
            model_path: path_str,
            stats: InferenceStats::default(),
        })
    }
    
    /// 執行推理
    /// 
    /// # 參數
    /// - `features`: 特徵張量，shape 應該符合模型預期的輸入格式
    /// 
    /// # 返回
    /// - 推理結果，包含預測的動作、倉位大小和置信度
    pub fn infer(&mut self, features: &Tensor) -> Result<InferenceResult> {
        let start = Instant::now();
        
        // 確保張量在正確的設備上
        let input = features.to_device(self.device);
        
        // 執行前向傳播
        let output = self.model.forward_ts(&[input])
            .context("Failed to run model inference")?;
        
        // 解析輸出（假設模型輸出 [action_logits, position_size, confidence]）
        let result = self.parse_output(output)?;
        
        // 更新統計
        let latency_us = start.elapsed().as_micros() as u64;
        self.update_stats(latency_us);
        
        debug!("Inference completed in {}μs: {:?}", latency_us, result.action);
        
        Ok(InferenceResult {
            action: result.action,
            position_size: result.position_size,
            confidence: result.confidence,
            latency_us,
        })
    }
    
    /// 批量推理
    pub fn infer_batch(&mut self, features_batch: &Tensor) -> Result<Vec<InferenceResult>> {
        let start = Instant::now();
        let batch_size = features_batch.size()[0] as usize;
        
        // 確保張量在正確的設備上
        let input = features_batch.to_device(self.device);
        
        // 執行批量前向傳播
        let outputs = self.model.forward_ts(&[input])
            .context("Failed to run batch inference")?;
        
        // 解析批量輸出
        let mut results = Vec::with_capacity(batch_size);
        for i in 0..batch_size {
            let result = self.parse_output_at_index(&outputs, i)?;
            results.push(result);
        }
        
        // 更新統計
        let total_latency_us = start.elapsed().as_micros() as u64;
        let per_sample_latency = total_latency_us / batch_size as u64;
        self.update_stats(per_sample_latency);
        
        debug!("Batch inference ({} samples) completed in {}μs", batch_size, total_latency_us);
        
        Ok(results)
    }
    
    /// 解析模型輸出
    fn parse_output(&self, output: Tensor) -> Result<InferenceResult> {
        // 假設輸出格式為 [action_logits(3), position_size(1), confidence(1)]
        let output_vec: Vec<f64> = output.try_into()?;
        
        if output_vec.len() < 5 {
            return Err(anyhow::anyhow!("Invalid model output shape"));
        }
        
        // 解析動作（前3個值是 action logits）
        let action_logits = &output_vec[0..3];
        let max_idx = action_logits
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
            .map(|(idx, _)| idx)
            .unwrap_or(2); // 默認 Hold
        
        let action = match max_idx {
            0 => TradingAction::Buy,
            1 => TradingAction::Sell,
            _ => TradingAction::Hold,
        };
        
        // 解析倉位大小和置信度
        let position_size = output_vec[3].clamp(-1.0, 1.0);
        let confidence = output_vec[4].clamp(0.0, 1.0);
        
        Ok(InferenceResult {
            action,
            position_size,
            confidence,
            latency_us: 0, // 將在調用函數中設置
        })
    }
    
    /// 解析批量輸出中的指定索引
    fn parse_output_at_index(&self, outputs: &Tensor, index: usize) -> Result<InferenceResult> {
        let output_slice = outputs.get(index as i64);
        self.parse_output(output_slice)
    }
    
    /// 更新推理統計
    fn update_stats(&mut self, latency_us: u64) {
        self.stats.total_inferences += 1;
        self.stats.total_latency_us += latency_us;
        
        if self.stats.min_latency_us == 0 || latency_us < self.stats.min_latency_us {
            self.stats.min_latency_us = latency_us;
        }
        
        if latency_us > self.stats.max_latency_us {
            self.stats.max_latency_us = latency_us;
        }
    }
    
    /// 獲取推理統計
    pub fn get_stats(&self) -> InferenceStatsSummary {
        let avg_latency_us = if self.stats.total_inferences > 0 {
            self.stats.total_latency_us / self.stats.total_inferences
        } else {
            0
        };
        
        InferenceStatsSummary {
            total_inferences: self.stats.total_inferences,
            avg_latency_us,
            max_latency_us: self.stats.max_latency_us,
            min_latency_us: self.stats.min_latency_us,
            model_path: self.model_path.clone(),
        }
    }
    
    /// 重新加載模型（用於模型更新）
    pub fn reload_model(&mut self) -> Result<()> {
        info!("Reloading TorchScript model from {}", self.model_path);
        
        let new_model = CModule::load_on_device(&self.model_path, self.device)
            .with_context(|| format!("Failed to reload model from {}", self.model_path))?;
        
        self.model = new_model;
        self.stats = InferenceStats::default(); // 重置統計
        
        info!("Model reloaded successfully");
        Ok(())
    }
}

/// 推理統計摘要
#[derive(Debug, Serialize, Deserialize)]
pub struct InferenceStatsSummary {
    pub total_inferences: u64,
    pub avg_latency_us: u64,
    pub max_latency_us: u64,
    pub min_latency_us: u64,
    pub model_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_trading_action() {
        // 測試 action 枚舉
        let action = TradingAction::Buy;
        assert!(matches!(action, TradingAction::Buy));
    }
    
    // 注意：實際的推理測試需要有效的 TorchScript 模型文件
}