"""
特征合约验证器
基于Design by Contract原则验证特征一致性
"""

import torch
import pandas as pd
import numpy as np
from typing import Dict, List, Any, Optional, Tuple
from pathlib import Path
import json

from .feature_registry import FeatureRegistry


class FeatureValidator:
    """特征合约验证器"""

    def __init__(self, registry: Optional[FeatureRegistry] = None):
        self.registry = registry or FeatureRegistry()

    @staticmethod
    def validate_training_data(data: pd.DataFrame, feature_set_name: str,
                             registry: Optional[FeatureRegistry] = None) -> Dict[str, Any]:
        """验证训练数据特征合约"""
        if registry is None:
            registry = FeatureRegistry()

        result = {
            "valid": False,
            "issues": [],
            "statistics": {}
        }

        # 获取预期特征集
        feature_set = registry.get_feature_set(feature_set_name)
        if not feature_set:
            result["issues"].append(f"特征集 {feature_set_name} 不存在")
            return result

        expected_features = [f.name for f in feature_set.features]

        # 排除非特征列
        exclude_cols = ['exchange_ts', 'symbol', 'mid_price']
        actual_features = [col for col in data.columns if col not in exclude_cols]

        # 验证特征数量
        if len(actual_features) != len(expected_features):
            result["issues"].append(
                f"特征数量不匹配: 期望{len(expected_features)}, 实际{len(actual_features)}"
            )

        # 验证特征名称
        missing_features = set(expected_features) - set(actual_features)
        extra_features = set(actual_features) - set(expected_features)

        if missing_features:
            result["issues"].append(f"缺失特征: {list(missing_features)}")

        if extra_features:
            result["issues"].append(f"多余特征: {list(extra_features)}")

        # 验证数据质量
        common_features = set(actual_features) & set(expected_features)
        quality_issues = []

        for feature in common_features:
            if feature in data.columns:
                series = data[feature]

                # 检查缺失值
                missing_pct = series.isnull().sum() / len(series) * 100
                if missing_pct > 5:
                    quality_issues.append(f"{feature}: {missing_pct:.1f}%缺失值")

                # 检查常数特征
                if series.std() < 1e-10:
                    quality_issues.append(f"{feature}: 常数特征(std={series.std():.2e})")

                # 检查异常值
                if np.isfinite(series).all():
                    q99 = series.quantile(0.99)
                    q01 = series.quantile(0.01)
                    outliers = ((series > q99 * 10) | (series < q01 * 10)).sum()
                    if outliers > len(series) * 0.01:
                        quality_issues.append(f"{feature}: {outliers}个异常值")

        if quality_issues:
            result["issues"].extend(quality_issues)

        # 统计信息
        result["statistics"] = {
            "rows": len(data),
            "expected_features": len(expected_features),
            "actual_features": len(actual_features),
            "common_features": len(common_features),
            "data_quality_issues": len(quality_issues)
        }

        result["valid"] = len(result["issues"]) == 0
        return result

    @staticmethod
    def validate_model_input(model_path: str, feature_set_name: str,
                           registry: Optional[FeatureRegistry] = None) -> Dict[str, Any]:
        """验证模型输入维度合约"""
        if registry is None:
            registry = FeatureRegistry()

        result = {
            "valid": False,
            "issues": [],
            "model_info": {}
        }

        try:
            # 加载模型
            checkpoint = torch.load(model_path, map_location='cpu', weights_only=False)
            state_dict = checkpoint.get('model_state_dict', checkpoint)

            # 推断输入维度
            input_dim = None
            for key, tensor in state_dict.items():
                if any(pattern in key.lower() for pattern in ['conv1.weight', 'linear.weight', 'embed.weight']):
                    if len(tensor.shape) >= 2:
                        input_dim = tensor.shape[1] if 'conv' in key else tensor.shape[-1]
                        break

            if input_dim is None:
                result["issues"].append("无法从模型推断输入维度")
                return result

            # 获取预期特征集
            feature_set = registry.get_feature_set(feature_set_name)
            if not feature_set:
                result["issues"].append(f"特征集 {feature_set_name} 不存在")
                return result

            expected_dim = len(feature_set.features)

            # 验证维度匹配
            if input_dim != expected_dim:
                result["issues"].append(
                    f"模型输入维度不匹配: 模型期望{input_dim}, 特征集提供{expected_dim}"
                )

            # 模型信息
            result["model_info"] = {
                "model_path": model_path,
                "input_dimension": input_dim,
                "expected_dimension": expected_dim,
                "feature_set": feature_set_name,
                "model_size_mb": Path(model_path).stat().st_size / 1024 / 1024
            }

            result["valid"] = len(result["issues"]) == 0

        except Exception as e:
            result["issues"].append(f"模型加载失败: {e}")

        return result

    @staticmethod
    def validate_pipeline_consistency(training_features: List[str], model_path: str,
                                    feature_set_name: str,
                                    registry: Optional[FeatureRegistry] = None) -> Dict[str, Any]:
        """验证整个流水线的特征一致性"""
        if registry is None:
            registry = FeatureRegistry()

        result = {
            "valid": False,
            "issues": [],
            "consistency_report": {}
        }

        # 验证训练特征
        feature_set = registry.get_feature_set(feature_set_name)
        if not feature_set:
            result["issues"].append(f"特征集 {feature_set_name} 不存在")
            return result

        expected_features = [f.name for f in feature_set.features]

        # 检查训练特征一致性
        training_issues = []
        if len(training_features) != len(expected_features):
            training_issues.append(
                f"训练特征数量不匹配: {len(training_features)} vs {len(expected_features)}"
            )

        missing_in_training = set(expected_features) - set(training_features)
        if missing_in_training:
            training_issues.append(f"训练中缺失特征: {list(missing_in_training)}")

        # 验证模型输入
        model_validation = FeatureValidator.validate_model_input(
            model_path, feature_set_name, registry
        )

        # 汇总结果
        all_issues = training_issues + model_validation.get("issues", [])
        result["issues"] = all_issues

        result["consistency_report"] = {
            "training_features_count": len(training_features),
            "expected_features_count": len(expected_features),
            "model_input_dimension": model_validation.get("model_info", {}).get("input_dimension"),
            "all_consistent": len(all_issues) == 0
        }

        result["valid"] = len(all_issues) == 0
        return result

    def generate_fix_suggestions(self, validation_result: Dict[str, Any]) -> List[str]:
        """根据验证结果生成修复建议"""
        suggestions = []

        for issue in validation_result.get("issues", []):
            if "特征数量不匹配" in issue:
                suggestions.append("使用特征注册表标准化特征维度")

            elif "缺失特征" in issue:
                suggestions.append("添加缺失特征或更新特征集定义")

            elif "多余特征" in issue:
                suggestions.append("移除多余特征或更新特征集定义")

            elif "常数特征" in issue:
                suggestions.append("检查特征计算逻辑，修复常数特征问题")

            elif "异常值" in issue:
                suggestions.append("添加数据清洗和异常值处理")

            elif "模型输入维度不匹配" in issue:
                suggestions.append("重新训练模型使用正确的特征维度")

            elif "无法推断输入维度" in issue:
                suggestions.append("检查模型结构，确保第一层参数可识别")

        if not suggestions:
            suggestions.append("验证通过，无需修复")

        return suggestions


def create_validation_report(data_path: str, model_path: str,
                           feature_set_name: str = "unified_v2") -> Dict[str, Any]:
    """创建完整的验证报告"""
    registry = FeatureRegistry()
    validator = FeatureValidator(registry)

    report = {
        "timestamp": pd.Timestamp.now().isoformat(),
        "feature_set": feature_set_name,
        "validations": {}
    }

    # 验证数据
    if Path(data_path).exists():
        if data_path.endswith('.parquet'):
            data = pd.read_parquet(data_path)
        else:
            data = pd.read_csv(data_path)

        data_validation = FeatureValidator.validate_training_data(
            data, feature_set_name, registry
        )
        report["validations"]["training_data"] = data_validation

        # 提取特征名
        exclude_cols = ['exchange_ts', 'symbol', 'mid_price']
        training_features = [col for col in data.columns if col not in exclude_cols]
    else:
        training_features = []
        report["validations"]["training_data"] = {
            "valid": False,
            "issues": [f"数据文件不存在: {data_path}"]
        }

    # 验证模型
    if Path(model_path).exists():
        model_validation = FeatureValidator.validate_model_input(
            model_path, feature_set_name, registry
        )
        report["validations"]["model"] = model_validation
    else:
        report["validations"]["model"] = {
            "valid": False,
            "issues": [f"模型文件不存在: {model_path}"]
        }

    # 验证流水线一致性
    if training_features and Path(model_path).exists():
        pipeline_validation = FeatureValidator.validate_pipeline_consistency(
            training_features, model_path, feature_set_name, registry
        )
        report["validations"]["pipeline"] = pipeline_validation

    # 生成修复建议
    all_issues = []
    for validation in report["validations"].values():
        all_issues.extend(validation.get("issues", []))

    if all_issues:
        report["fix_suggestions"] = validator.generate_fix_suggestions(
            {"issues": all_issues}
        )
    else:
        report["fix_suggestions"] = ["所有验证通过，系统状态良好"]

    return report


if __name__ == "__main__":
    # 创建示例验证报告
    print("🔍 创建特征验证示例...")

    # 使用实际文件路径
    data_path = "cache/ETHUSDT_enhanced_2025-09-01_2025-09-08_time_fixed.parquet"
    model_path = "models/tcn_ETH_clean_data_small.pt"

    if Path(data_path).exists() and Path(model_path).exists():
        report = create_validation_report(data_path, model_path)

        # 保存报告
        with open("feature_validation_report.json", 'w', encoding='utf-8') as f:
            json.dump(report, f, indent=2, ensure_ascii=False)

        print("✅ 特征验证报告已创建: feature_validation_report.json")

        # 打印关键结果
        for validation_type, result in report["validations"].items():
            status = "✅" if result["valid"] else "❌"
            print(f"{status} {validation_type}: {len(result.get('issues', []))}个问题")

    else:
        print("⚠️ 测试文件不存在，跳过验证示例")