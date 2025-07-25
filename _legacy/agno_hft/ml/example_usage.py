"""
ML 模塊使用示例
=============

展示如何使用完整的 ML 訓練管道
"""

import logging
from datetime import datetime, timedelta

# 本地導入
from data.data_loader import DataLoader as HFTDataLoader
from data.labeling import LabelGenerator
from data.preprocessing import LOBPreprocessor
from models.tlob import TLOB, TLOBConfig
from trainer import TLOBTrainerImplementation, create_optimizer, create_scheduler
from dataset import TLOBDataModule
from export import ModelExporter

# 設置日誌
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


def example_tlob_training():
    """完整的 TLOB 訓練示例"""
    
    # 1. 配置
    config = {
        'symbol': 'BTCUSDT',
        'start_time': '2024-01-01',
        'end_time': '2024-01-31',
        
        # 模型配置
        'd_model': 256,
        'nhead': 8,
        'num_layers': 4,
        'dropout': 0.1,
        
        # 訓練配置
        'batch_size': 64,
        'learning_rate': 1e-4,
        'epochs': 50,
        'train_ratio': 0.7,
        'val_ratio': 0.15,
        
        # 其他
        'use_tensorboard': True,
        'output_dir': './outputs/example_run'
    }
    
    logger.info("Starting TLOB training example...")
    
    # 2. 數據準備
    data_module = TLOBDataModule(
        data_config=config,
        batch_size=config['batch_size']
    )
    data_module.setup()
    
    # 獲取數據加載器
    train_loader = data_module.train_dataloader()
    val_loader = data_module.val_dataloader()
    
    # 3. 模型創建
    input_dim = train_loader.dataset.sequences.shape[-1]
    model_config = TLOBConfig(
        input_dim=input_dim,
        d_model=config['d_model'],
        nhead=config['nhead'],
        num_layers=config['num_layers'],
        dropout=config['dropout']
    )
    
    model = TLOB(model_config)
    
    # 4. 優化器和調度器
    optimizer = create_optimizer(model, config)
    scheduler = create_scheduler(optimizer, config)
    
    # 5. 訓練器
    trainer = TLOBTrainerImplementation(
        model=model,
        train_loader=train_loader,
        val_loader=val_loader,
        optimizer=optimizer,
        scheduler=scheduler,
        output_dir=config['output_dir'],
        config=config
    )
    
    # 6. 訓練
    results = trainer.train(
        epochs=config['epochs'],
        early_stopping_patience=10
    )
    
    # 7. 模型導出
    if trainer.best_model_path:
        exporter = ModelExporter(config['output_dir'] + '/exported_models')
        
        # 加載最佳模型
        trainer.load_checkpoint(trainer.best_model_path)
        
        # 創建示例輸入
        example_input = train_loader.dataset.sequences[:1]
        
        # 導出為 TorchScript
        export_info = exporter.export_for_rust(
            trainer.model,
            f"tlob_{config['symbol']}",
            example_input
        )
        
        logger.info(f"Model exported to: {export_info['model_path']}")
    
    logger.info("Training example completed!")
    return results


def example_data_processing():
    """數據處理示例"""
    
    logger.info("Data processing example...")
    
    # 1. 加載原始數據
    data_loader = HFTDataLoader()
    
    # 注意：這裡需要實際的數據，示例中使用模擬數據
    logger.info("Loading raw LOB data...")
    # raw_data = data_loader.load_orderbook_data(...)
    
    # 2. 標籤生成
    label_generator = LabelGenerator()
    logger.info("Generating labels...")
    # labeled_data = label_generator.generate_labels(raw_data)
    
    # 3. 數據預處理
    preprocessor = LOBPreprocessor()
    logger.info("Preprocessing data...")
    # preprocessor.fit(train_data)
    # sequences = preprocessor.transform(test_data)
    
    # 4. 保存預處理統計信息
    # preprocessor.save_normalization_stats('./normalization_stats.json')
    
    logger.info("Data processing example completed!")


def example_model_evaluation():
    """模型評估示例"""
    
    logger.info("Model evaluation example...")
    
    # 這裡需要一個已訓練的模型
    model_path = './outputs/example_run/best_model.pt'
    
    try:
        # 加載模型
        checkpoint = torch.load(model_path, map_location='cpu')
        model_config = TLOBConfig(**checkpoint['model_config'])
        model = TLOB(model_config)
        model.load_state_dict(checkpoint['model_state_dict'])
        
        logger.info("Model loaded successfully")
        logger.info(f"Model info: {model.get_model_info()}")
        
        # 進行評估...
        # 這裡可以添加實際的評估代碼
        
    except FileNotFoundError:
        logger.warning("No trained model found. Run training first.")
    
    logger.info("Model evaluation example completed!")


if __name__ == '__main__':
    print("=== ML 模塊使用示例 ===")
    print("1. 數據處理示例")
    print("2. TLOB 訓練示例")  
    print("3. 模型評估示例")
    
    choice = input("選擇示例 (1-3): ")
    
    if choice == '1':
        example_data_processing()
    elif choice == '2':
        example_tlob_training()
    elif choice == '3':
        example_model_evaluation()
    else:
        print("無效選擇")
        
    print("示例完成！")