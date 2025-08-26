//! TorchScript 模型加載器 (tch-rs 集成)

use crate::config::ModelConfig;
use hft_core::{HftResult, HftError, Timestamp};
use std::sync::Arc;
use tracing::{info, warn, error};

#[cfg(feature = "torchscript")]
use tch::{CModule, Device, Tensor};

/// 模型句柄 - 統一的模型接口
#[derive(Debug)]
pub struct ModelHandle {
    #[cfg(feature = "torchscript")]
    module: CModule,
    
    #[cfg(not(feature = "torchscript"))]
    mock_model: MockModel,
    
    config: ModelConfig,
    device: ModelDevice,
    load_time: Timestamp,
    inference_count: std::sync::atomic::AtomicU64,
}

/// 設備抽象
#[derive(Debug, Clone)]
pub enum ModelDevice {
    #[cfg(feature = "torchscript")]
    Torch(Device),
    
    Cpu,
    Mock,
}

/// Mock 模型 (用於測試或無 GPU 環境)
#[derive(Debug)]
struct MockModel {
    input_dim: usize,
    output_dim: usize,
}

/// 模型加載器
pub struct ModelLoader;

impl ModelLoader {
    /// 加載 TorchScript 模型
    pub fn load_model(config: ModelConfig) -> HftResult<Arc<ModelHandle>> {
        info!("開始加載 DL 模型: {:?}", config.model_path);
        
        let start_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        
        #[cfg(feature = "torchscript")]
        {
            Self::load_torchscript_model(config, start_time)
        }
        
        #[cfg(not(feature = "torchscript"))]
        {
            Self::load_mock_model(config, start_time)
        }
    }
    
    #[cfg(feature = "torchscript")]
    fn load_torchscript_model(config: ModelConfig, start_time: Timestamp) -> HftResult<Arc<ModelHandle>> {
        // 解析設備
        let device = Self::parse_device(&config.device)?;
        
        // 加載 TorchScript 模型
        let module = CModule::load(&config.model_path)
            .map_err(|e| HftError::Config(format!("TorchScript 模型加載失敗: {}", e)))?;
        
        // 驗證模型 (可選的熱身推理)
        if let Err(e) = Self::validate_torchscript_model(&module, &config, &device) {
            warn!("模型驗證失敗，但仍繼續加載: {}", e);
        }
        
        let load_duration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64 - start_time;
        
        info!("TorchScript 模型加載完成，耗時: {}μs", load_duration);
        
        Ok(Arc::new(ModelHandle {
            module,
            config,
            device: ModelDevice::Torch(device),
            load_time: start_time,
            inference_count: std::sync::atomic::AtomicU64::new(0),
        }))
    }
    
    #[cfg(not(feature = "torchscript"))]
    fn load_mock_model(config: ModelConfig, start_time: Timestamp) -> HftResult<Arc<ModelHandle>> {
        warn!("TorchScript 功能未啟用，使用 Mock 模型");
        
        let mock_model = MockModel {
            input_dim: config.input_dim,
            output_dim: config.output_dim,
        };
        
        Ok(Arc::new(ModelHandle {
            mock_model,
            config,
            device: ModelDevice::Mock,
            load_time: start_time,
            inference_count: std::sync::atomic::AtomicU64::new(0),
        }))
    }
    
    #[cfg(feature = "torchscript")]
    fn parse_device(device_str: &str) -> HftResult<Device> {
        match device_str.to_lowercase().as_str() {
            "cpu" => Ok(Device::Cpu),
            "auto" => {
                if tch::Cuda::is_available() {
                    info!("檢測到 CUDA，使用 GPU");
                    Ok(Device::Cuda(0))
                } else {
                    info!("未檢測到 CUDA，使用 CPU");
                    Ok(Device::Cpu)
                }
            }
            s if s.starts_with("cuda:") => {
                let gpu_id: i64 = s[5..].parse()
                    .map_err(|_| HftError::Config(format!("無效的 GPU ID: {}", s)))?;
                
                if tch::Cuda::is_available() && gpu_id < tch::Cuda::device_count() {
                    Ok(Device::Cuda(gpu_id))
                } else {
                    Err(HftError::Config(format!("GPU {} 不可用", gpu_id)))
                }
            }
            _ => Err(HftError::Config(format!("不支持的設備: {}", device_str)))
        }
    }
    
    #[cfg(feature = "torchscript")]
    fn validate_torchscript_model(
        module: &CModule, 
        config: &ModelConfig, 
        device: &Device
    ) -> HftResult<()> {
        // 創建測試輸入張量
        let test_input = Tensor::randn(
            &[config.batch_size as i64, config.input_dim as i64], 
            (tch::Kind::Float, *device)
        );
        
        // 執行前向推理
        let output = module.forward_ts(&[test_input])
            .map_err(|e| HftError::Execution(format!("模型推理失敗: {}", e)))?;
        
        // 驗證輸出維度
        let output_shape = output.size();
        if output_shape.len() != 2 || 
           output_shape[0] != config.batch_size as i64 || 
           output_shape[1] != config.output_dim as i64 {
            return Err(HftError::Config(format!(
                "模型輸出維度不匹配: 期望 [{}, {}]，實際 {:?}",
                config.batch_size, config.output_dim, output_shape
            )));
        }
        
        info!("模型驗證通過: 輸入 [{}, {}] → 輸出 {:?}", 
              config.batch_size, config.input_dim, output_shape);
        Ok(())
    }
}

impl ModelHandle {
    /// 執行推理
    pub fn predict(&self, input: &[f32]) -> HftResult<Vec<f32>> {
        if input.len() != self.config.input_dim {
            return Err(HftError::InvalidOrder(format!(
                "輸入維度不匹配: 期望 {}，實際 {}", 
                self.config.input_dim, input.len()
            )));
        }
        
        // 增加推理計數
        self.inference_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        
        #[cfg(feature = "torchscript")]
        {
            self.predict_torchscript(input)
        }
        
        #[cfg(not(feature = "torchscript"))]
        {
            self.predict_mock(input)
        }
    }
    
    #[cfg(feature = "torchscript")]
    fn predict_torchscript(&self, input: &[f32]) -> HftResult<Vec<f32>> {
        let device = match &self.device {
            ModelDevice::Torch(d) => d,
            _ => return Err(HftError::Execution("設備類型錯誤".to_string())),
        };
        
        // 轉換輸入為張量
        let input_tensor = Tensor::of_slice(input)
            .reshape(&[1, self.config.input_dim as i64])
            .to_device(*device);
        
        // 執行推理
        let output = self.module.forward_ts(&[input_tensor])
            .map_err(|e| HftError::Execution(format!("推理執行失敗: {}", e)))?;
        
        // 轉換輸出為 Vec<f32>
        let output_vec = Vec::<f32>::from(output.flatten(0, -1))
            .into_iter()
            .collect();
        
        Ok(output_vec)
    }
    
    #[cfg(not(feature = "torchscript"))]
    fn predict_mock(&self, input: &[f32]) -> HftResult<Vec<f32>> {
        // Mock 預測: 簡單的線性組合 + tanh
        let sum: f32 = input.iter().take(10).sum(); // 取前 10 個特徵
        let output = (sum / 10.0).tanh(); // 歸一化並應用 tanh
        
        Ok(vec![output])
    }
    
    /// 獲取模型統計信息
    pub fn get_stats(&self) -> ModelStats {
        ModelStats {
            load_time: self.load_time,
            inference_count: self.inference_count.load(std::sync::atomic::Ordering::Relaxed),
            device: format!("{:?}", self.device),
            model_path: self.config.model_path.clone(),
            input_dim: self.config.input_dim,
            output_dim: self.config.output_dim,
        }
    }
    
    /// 熱重載模型 (保持相同配置)
    pub fn reload(&mut self) -> HftResult<()> {
        info!("開始熱重載模型: {:?}", self.config.model_path);
        
        #[cfg(feature = "torchscript")]
        {
            let new_module = CModule::load(&self.config.model_path)
                .map_err(|e| HftError::Config(format!("模型重載失敗: {}", e)))?;
            
            self.module = new_module;
            self.load_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as u64;
            
            info!("模型熱重載完成");
        }
        
        #[cfg(not(feature = "torchscript"))]
        {
            warn!("Mock 模型無需重載");
        }
        
        Ok(())
    }
}

/// 模型統計信息
#[derive(Debug, Clone)]
pub struct ModelStats {
    pub load_time: Timestamp,
    pub inference_count: u64,
    pub device: String,
    pub model_path: std::path::PathBuf,
    pub input_dim: usize,
    pub output_dim: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn create_test_config() -> ModelConfig {
        ModelConfig {
            model_path: PathBuf::from("test_model.pt"), // 不存在的文件，用於測試
            model_version: Some("test_v1".to_string()),
            device: "cpu".to_string(),
            batch_size: 1,
            input_dim: 10,
            output_dim: 1,
        }
    }

    #[test]
    fn test_mock_model_prediction() {
        let config = create_test_config();
        
        // 在無 TorchScript 環境下應該創建 Mock 模型
        if let Ok(model) = ModelLoader::load_model(config) {
            let input = vec![0.1; 10]; // 10 維輸入
            let result = model.predict(&input);
            
            // Mock 模型應該能正常預測
            assert!(result.is_ok());
            let output = result.unwrap();
            assert_eq!(output.len(), 1);
            assert!(output[0] >= -1.0 && output[0] <= 1.0); // tanh 輸出範圍
        }
    }

    #[test]
    fn test_input_dimension_validation() {
        let config = create_test_config();
        
        if let Ok(model) = ModelLoader::load_model(config) {
            // 錯誤的輸入維度應該失敗
            let wrong_input = vec![0.1; 5]; // 5 維輸入，但期望 10 維
            let result = model.predict(&wrong_input);
            assert!(result.is_err());
        }
    }

    #[cfg(feature = "torchscript")]
    #[test]
    fn test_device_parsing() {
        assert!(ModelLoader::parse_device("cpu").is_ok());
        assert!(ModelLoader::parse_device("auto").is_ok());
        
        // 無效設備應該失敗
        assert!(ModelLoader::parse_device("invalid").is_err());
    }
}