/*!
 * AgnoAgent 核心系統
 * 
 * 常駐的智能Agent，與HFT系統並行運行
 */

use std::sync::Arc;
use tokio::sync::{RwLock, mpsc, Mutex};
use tokio::time::{Duration, sleep, Instant};
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use tracing::{info, warn, error, debug};

use crate::agent::{
    AgentProtocol, 
    ConversationInterface, 
    TaskScheduler, 
    SystemMonitor, 
    DecisionEngine,
    AgentConfig
};

/// Agent系統狀態
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentState {
    Initializing,
    Ready,
    Planning,
    Executing,
    Monitoring,
    Error(String),
    Shutdown,
}

/// HFT系統狀態
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HftSystemStatus {
    pub training_active: bool,
    pub evaluation_active: bool,
    pub dryrun_active: bool,
    pub live_trading_active: bool,
    pub last_update: Instant,
    pub current_symbol: Option<String>,
    pub performance_metrics: HashMap<String, f64>,
}

/// Agent與用戶的對話上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationContext {
    pub user_intent: String,
    pub current_task: Option<String>,
    pub trading_symbol: Option<String>,
    pub capital_amount: Option<f64>,
    pub risk_level: Option<String>,
    pub session_id: String,
}

/// 主要的AgnoAgent結構
#[derive(Debug)]
pub struct AgnoAgent {
    /// Agent配置
    config: Arc<AgentConfig>,
    
    /// Agent狀態
    state: Arc<RwLock<AgentState>>,
    
    /// HFT系統狀態
    hft_status: Arc<RwLock<HftSystemStatus>>,
    
    /// 對話界面
    conversation: Arc<ConversationInterface>,
    
    /// 任務調度器
    scheduler: Arc<TaskScheduler>,
    
    /// 系統監控器
    monitor: Arc<SystemMonitor>,
    
    /// 決策引擎
    decision_engine: Arc<DecisionEngine>,
    
    /// 與HFT系統的通信協議
    protocol: Arc<AgentProtocol>,
    
    /// 當前對話上下文
    context: Arc<RwLock<ConversationContext>>,
    
    /// 命令通道
    command_tx: mpsc::UnboundedSender<AgentCommand>,
    command_rx: Arc<Mutex<mpsc::UnboundedReceiver<AgentCommand>>>,
}

/// Agent命令類型
#[derive(Debug, Clone)]
pub enum AgentCommand {
    /// 用戶輸入命令
    UserInput(String),
    
    /// 系統狀態更新
    SystemUpdate(HftSystemStatus),
    
    /// 執行訓練任務
    ExecuteTraining {
        symbol: String,
        hours: u32,
        parameters: HashMap<String, serde_json::Value>,
    },
    
    /// 執行評估任務
    ExecuteEvaluation {
        model_path: String,
        symbol: String,
        criteria: HashMap<String, f64>,
    },
    
    /// 執行乾跑測試
    ExecuteDryRun {
        symbol: String,
        capital: f64,
        duration: Duration,
        config: HashMap<String, serde_json::Value>,
    },
    
    /// 執行實盤交易
    ExecuteLiveTrading {
        symbol: String,
        capital: f64,
        config: HashMap<String, serde_json::Value>,
    },
    
    /// 停止所有任務
    StopAll,
    
    /// 緊急停止
    EmergencyStop,
    
    /// 關閉Agent
    Shutdown,
}

impl AgnoAgent {
    /// 創建新的AgnoAgent實例
    pub async fn new(config: AgentConfig) -> Result<Self, Box<dyn std::error::Error>> {
        info!("🤖 初始化AgnoAgent...");
        
        let config = Arc::new(config);
        let state = Arc::new(RwLock::new(AgentState::Initializing));
        
        // 初始化HFT系統狀態
        let hft_status = Arc::new(RwLock::new(HftSystemStatus {
            training_active: false,
            evaluation_active: false,
            dryrun_active: false,
            live_trading_active: false,
            last_update: Instant::now(),
            current_symbol: None,
            performance_metrics: HashMap::new(),
        }));
        
        // 創建命令通道
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let command_rx = Arc::new(Mutex::new(command_rx));
        
        // 初始化對話上下文
        let context = Arc::new(RwLock::new(ConversationContext {
            user_intent: String::new(),
            current_task: None,
            trading_symbol: None,
            capital_amount: None,
            risk_level: None,
            session_id: uuid::Uuid::new_v4().to_string(),
        }));
        
        // 初始化各個組件
        let conversation = Arc::new(ConversationInterface::new().await?);
        let scheduler = Arc::new(TaskScheduler::new().await?);
        let monitor = Arc::new(SystemMonitor::new().await?);
        let decision_engine = Arc::new(DecisionEngine::new().await?);
        let protocol = Arc::new(AgentProtocol::new().await?);
        
        let agent = Self {
            config,
            state,
            hft_status,
            conversation,
            scheduler,
            monitor,
            decision_engine,
            protocol,
            context,
            command_tx,
            command_rx,
        };
        
        info!("✅ AgnoAgent初始化完成");
        Ok(agent)
    }
    
    /// 啟動Agent常駐服務
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("🚀 啟動AgnoAgent常駐服務...");
        
        // 更新狀態為Ready
        {
            let mut state = self.state.write().await;
            *state = AgentState::Ready;
        }
        
        // 啟動各個子系統
        let agent_clone = self.clone();
        tokio::spawn(async move {
            if let Err(e) = agent_clone.run_command_processor().await {
                error!("命令處理器錯誤: {}", e);
            }
        });
        
        let agent_clone = self.clone();
        tokio::spawn(async move {
            if let Err(e) = agent_clone.run_conversation_loop().await {
                error!("對話循環錯誤: {}", e);
            }
        });
        
        let agent_clone = self.clone();
        tokio::spawn(async move {
            if let Err(e) = agent_clone.run_monitoring_loop().await {
                error!("監控循環錯誤: {}", e);
            }
        });
        
        info!("✅ AgnoAgent所有子系統已啟動");
        
        // 顯示歡迎信息
        self.show_welcome_message().await;
        
        Ok(())
    }
    
    /// 顯示歡迎信息
    async fn show_welcome_message(&self) {
        println!("\n🤖 ================================");
        println!("🤖   AgnoAgent HFT系統已啟動");
        println!("🤖 ================================");
        println!("🤖 您可以與我對話來管理您的交易系統：");
        println!("🤖");
        println!("🤖 例如：");
        println!("🤖 - \"訓練SOLUSDT模型\"");
        println!("🤖 - \"評估模型性能\"");
        println!("🤖 - \"開始乾跑測試\"");
        println!("🤖 - \"部署實盤交易\"");
        println!("🤖 - \"系統狀態\"");
        println!("🤖 - \"停止交易\"");
        println!("🤖");
        println!("🤖 輸入 'help' 查看更多命令");
        println!("🤖 輸入 'quit' 退出系統");
        println!("🤖 ================================");
        println!();
    }
    
    /// 命令處理循環
    async fn run_command_processor(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut command_rx = self.command_rx.lock().await;
        
        while let Some(command) = command_rx.recv().await {
            debug!("處理命令: {:?}", command);
            
            match command {
                AgentCommand::UserInput(input) => {
                    if let Err(e) = self.handle_user_input(&input).await {
                        error!("處理用戶輸入錯誤: {}", e);
                        println!("❌ 處理您的請求時出現錯誤: {}", e);
                    }
                }
                
                AgentCommand::SystemUpdate(status) => {
                    let mut hft_status = self.hft_status.write().await;
                    *hft_status = status;
                }
                
                AgentCommand::ExecuteTraining { symbol, hours, parameters } => {
                    if let Err(e) = self.execute_training(&symbol, hours, &parameters).await {
                        error!("執行訓練錯誤: {}", e);
                        println!("❌ 訓練任務失敗: {}", e);
                    }
                }
                
                AgentCommand::ExecuteEvaluation { model_path, symbol, criteria } => {
                    if let Err(e) = self.execute_evaluation(&model_path, &symbol, &criteria).await {
                        error!("執行評估錯誤: {}", e);
                        println!("❌ 評估任務失敗: {}", e);
                    }
                }
                
                AgentCommand::ExecuteDryRun { symbol, capital, duration, config } => {
                    if let Err(e) = self.execute_dryrun(&symbol, capital, duration, &config).await {
                        error!("執行乾跑錯誤: {}", e);
                        println!("❌ 乾跑任務失敗: {}", e);
                    }
                }
                
                AgentCommand::ExecuteLiveTrading { symbol, capital, config } => {
                    if let Err(e) = self.execute_live_trading(&symbol, capital, &config).await {
                        error!("執行實盤交易錯誤: {}", e);
                        println!("❌ 實盤交易任務失敗: {}", e);
                    }
                }
                
                AgentCommand::StopAll => {
                    if let Err(e) = self.stop_all_tasks().await {
                        error!("停止所有任務錯誤: {}", e);
                    }
                }
                
                AgentCommand::EmergencyStop => {
                    if let Err(e) = self.emergency_stop().await {
                        error!("緊急停止錯誤: {}", e);
                    }
                }
                
                AgentCommand::Shutdown => {
                    info!("收到關閉命令，正在關閉Agent...");
                    break;
                }
            }
        }
        
        Ok(())
    }
    
    /// 對話循環
    async fn run_conversation_loop(&self) -> Result<(), Box<dyn std::error::Error>> {
        use std::io::{self, Write};
        
        let mut stdin = tokio::io::stdin();
        let mut buffer = String::new();
        
        loop {
            print!("🤖 > ");
            io::stdout().flush()?;
            
            buffer.clear();
            let bytes_read = tokio::io::AsyncBufReadExt::read_line(&mut tokio::io::BufReader::new(&mut stdin), &mut buffer).await?;
            
            if bytes_read == 0 {
                break; // EOF
            }
            
            let input = buffer.trim().to_string();
            
            if input.is_empty() {
                continue;
            }
            
            if input == "quit" || input == "exit" {
                println!("🤖 再見！正在關閉系統...");
                let _ = self.command_tx.send(AgentCommand::Shutdown);
                break;
            }
            
            // 發送用戶輸入到命令處理器
            let _ = self.command_tx.send(AgentCommand::UserInput(input));
        }
        
        Ok(())
    }
    
    /// 監控循環
    async fn run_monitoring_loop(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        
        loop {
            interval.tick().await;
            
            // 監控HFT系統狀態
            if let Err(e) = self.monitor_hft_system().await {
                warn!("HFT系統監控錯誤: {}", e);
            }
            
            // 檢查Agent健康狀態
            if let Err(e) = self.check_agent_health().await {
                warn!("Agent健康檢查錯誤: {}", e);
            }
        }
    }
    
    /// 處理用戶輸入
    async fn handle_user_input(&self, input: &str) -> Result<(), Box<dyn std::error::Error>> {
        // 使用對話界面解析用戶意圖
        let intent = self.conversation.parse_user_intent(input).await?;
        
        // 更新對話上下文
        {
            let mut context = self.context.write().await;
            context.user_intent = intent.intent_type.clone();
            
            if let Some(symbol) = intent.extracted_symbol {
                context.trading_symbol = Some(symbol);
            }
            
            if let Some(capital) = intent.extracted_capital {
                context.capital_amount = Some(capital);
            }
        }
        
        // 使用決策引擎制定執行計劃
        let plan = self.decision_engine.create_execution_plan(&intent, &*self.context.read().await).await?;
        
        // 執行計劃
        self.execute_plan(plan).await?;
        
        Ok(())
    }
    
    /// 執行訓練任務（調用現有的訓練例子）
    async fn execute_training(&self, symbol: &str, hours: u32, parameters: &HashMap<String, serde_json::Value>) -> Result<(), Box<dyn std::error::Error>> {
        println!("🔄 開始訓練{}模型，訓練時長: {}小時", symbol, hours);
        
        // 更新狀態
        {
            let mut state = self.state.write().await;
            *state = AgentState::Executing;
        }
        
        {
            let mut hft_status = self.hft_status.write().await;
            hft_status.training_active = true;
            hft_status.current_symbol = Some(symbol.to_string());
        }
        
        // 調用現有的訓練系統
        let result = self.protocol.execute_training_command(symbol, hours, parameters).await?;
        
        // 更新狀態
        {
            let mut hft_status = self.hft_status.write().await;
            hft_status.training_active = false;
        }
        
        if result.success {
            println!("✅ {}模型訓練完成", symbol);
            println!("📊 模型路徑: {}", result.model_path.unwrap_or_default());
            println!("📈 訓練損失: {:.4}", result.final_loss.unwrap_or(0.0));
        } else {
            println!("❌ {}模型訓練失敗: {}", symbol, result.error_message.unwrap_or_default());
        }
        
        {
            let mut state = self.state.write().await;
            *state = AgentState::Ready;
        }
        
        Ok(())
    }
    
    /// 執行評估任務
    async fn execute_evaluation(&self, model_path: &str, symbol: &str, criteria: &HashMap<String, f64>) -> Result<(), Box<dyn std::error::Error>> {
        println!("🔄 開始評估{}模型性能", symbol);
        
        // 調用現有的評估系統
        let result = self.protocol.execute_evaluation_command(model_path, symbol, criteria).await?;
        
        if result.success {
            println!("✅ {}模型評估完成", symbol);
            if let Some(metrics) = result.performance_metrics {
                println!("📊 性能指標:");
                for (key, value) in metrics {
                    println!("   - {}: {:.4}", key, value);
                }
            }
        } else {
            println!("❌ {}模型評估失敗: {}", symbol, result.error_message.unwrap_or_default());
        }
        
        Ok(())
    }
    
    /// 執行乾跑測試
    async fn execute_dryrun(&self, symbol: &str, capital: f64, duration: Duration, config: &HashMap<String, serde_json::Value>) -> Result<(), Box<dyn std::error::Error>> {
        println!("🔄 開始{}乾跑測試，資金: {} USDT", symbol, capital);
        
        {
            let mut hft_status = self.hft_status.write().await;
            hft_status.dryrun_active = true;
            hft_status.current_symbol = Some(symbol.to_string());
        }
        
        // 調用現有的乾跑系統
        let result = self.protocol.execute_dryrun_command(symbol, capital, duration, config).await?;
        
        {
            let mut hft_status = self.hft_status.write().await;
            hft_status.dryrun_active = false;
        }
        
        if result.success {
            println!("✅ {}乾跑測試完成", symbol);
            if let Some(metrics) = result.performance_metrics {
                println!("📊 測試結果:");
                for (key, value) in metrics {
                    println!("   - {}: {:.4}", key, value);
                }
            }
        } else {
            println!("❌ {}乾跑測試失敗: {}", symbol, result.error_message.unwrap_or_default());
        }
        
        Ok(())
    }
    
    /// 執行實盤交易
    async fn execute_live_trading(&self, symbol: &str, capital: f64, config: &HashMap<String, serde_json::Value>) -> Result<(), Box<dyn std::error::Error>> {
        println!("⚠️  準備開始{}實盤交易，資金: {} USDT", symbol, capital);
        println!("🔒 請確認您已充分測試並理解風險");
        
        {
            let mut hft_status = self.hft_status.write().await;
            hft_status.live_trading_active = true;
            hft_status.current_symbol = Some(symbol.to_string());
        }
        
        // 調用現有的實盤交易系統
        let result = self.protocol.execute_live_trading_command(symbol, capital, config).await?;
        
        if result.success {
            println!("✅ {}實盤交易已啟動", symbol);
            println!("📊 交易狀態: 活躍");
            println!("💰 投入資金: {} USDT", capital);
        } else {
            println!("❌ {}實盤交易啟動失敗: {}", symbol, result.error_message.unwrap_or_default());
            
            let mut hft_status = self.hft_status.write().await;
            hft_status.live_trading_active = false;
        }
        
        Ok(())
    }
    
    /// 停止所有任務
    async fn stop_all_tasks(&self) -> Result<(), Box<dyn std::error::Error>> {
        println!("🛑 正在停止所有任務...");
        
        self.protocol.stop_all_commands().await?;
        
        {
            let mut hft_status = self.hft_status.write().await;
            hft_status.training_active = false;
            hft_status.evaluation_active = false;
            hft_status.dryrun_active = false;
            hft_status.live_trading_active = false;
        }
        
        println!("✅ 所有任務已停止");
        
        Ok(())
    }
    
    /// 緊急停止
    async fn emergency_stop(&self) -> Result<(), Box<dyn std::error::Error>> {
        println!("🚨 執行緊急停止...");
        
        self.protocol.emergency_stop_all().await?;
        
        {
            let mut hft_status = self.hft_status.write().await;
            hft_status.training_active = false;
            hft_status.evaluation_active = false;
            hft_status.dryrun_active = false;
            hft_status.live_trading_active = false;
        }
        
        {
            let mut state = self.state.write().await;
            *state = AgentState::Ready;
        }
        
        println!("✅ 緊急停止完成");
        
        Ok(())
    }
    
    /// 監控HFT系統
    async fn monitor_hft_system(&self) -> Result<(), Box<dyn std::error::Error>> {
        // 實現系統監控邏輯
        Ok(())
    }
    
    /// 檢查Agent健康狀態
    async fn check_agent_health(&self) -> Result<(), Box<dyn std::error::Error>> {
        // 實現健康檢查邏輯
        Ok(())
    }
    
    /// 執行計劃
    async fn execute_plan(&self, plan: crate::agent::ExecutionPlan) -> Result<(), Box<dyn std::error::Error>> {
        // 實現計劃執行邏輯
        Ok(())
    }
    
    /// 獲取系統狀態
    pub async fn get_system_status(&self) -> (AgentState, HftSystemStatus) {
        let agent_state = self.state.read().await.clone();
        let hft_status = self.hft_status.read().await.clone();
        (agent_state, hft_status)
    }
}

// 實現Clone trait以支持多線程使用
impl Clone for AgnoAgent {
    fn clone(&self) -> Self {
        Self {
            config: Arc::clone(&self.config),
            state: Arc::clone(&self.state),
            hft_status: Arc::clone(&self.hft_status),
            conversation: Arc::clone(&self.conversation),
            scheduler: Arc::clone(&self.scheduler),
            monitor: Arc::clone(&self.monitor),
            decision_engine: Arc::clone(&self.decision_engine),
            protocol: Arc::clone(&self.protocol),
            context: Arc::clone(&self.context),
            command_tx: self.command_tx.clone(),
            command_rx: Arc::clone(&self.command_rx),
        }
    }
}