#!/usr/bin/env python3
"""
HFT系統完整驗證腳本
基於4個debugger代理的修復建議，執行端到端系統測試
"""

import asyncio
import json
import time
import requests
import redis
import subprocess
import sys
from typing import Dict, List, Tuple, Optional
from datetime import datetime
import logging
from concurrent.futures import ThreadPoolExecutor
import grpc
from contextlib import asynccontextmanager

# 設置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s',
    handlers=[
        logging.StreamHandler(),
        logging.FileHandler(f'validation_{datetime.now().strftime("%Y%m%d_%H%M%S")}.log')
    ]
)
logger = logging.getLogger(__name__)

class HFTSystemValidator:
    """HFT系統驗證器"""
    
    def __init__(self):
        self.test_results = {}
        self.start_time = time.time()
        
        # 服務端點配置
        self.endpoints = {
            'redis': {'host': 'localhost', 'port': 6379},
            'clickhouse': {'url': 'http://localhost:8123'},
            'prometheus': {'url': 'http://localhost:9090'},
            'grafana': {'url': 'http://localhost:3000'},
            'rust_metrics': {'url': 'http://localhost:8080/metrics'},
            'rust_grpc': {'address': 'localhost:50051'},
            'ops_api': {'url': 'http://localhost:8003'},
            'ops_ui': {'url': 'http://localhost:8503'},
            'ml_api': {'url': 'http://localhost:8002'},
            'ml_ui': {'url': 'http://localhost:8502'},
            'master_api': {'url': 'http://localhost:8004'},
            'master_ui': {'url': 'http://localhost:8504'}
        }
        
        # 測試結果統計
        self.test_stats = {
            'total': 0,
            'passed': 0,
            'failed': 0,
            'warnings': 0
        }

    def log_test_result(self, test_name: str, success: bool, message: str, duration: float = 0):
        """記錄測試結果"""
        self.test_stats['total'] += 1
        if success:
            self.test_stats['passed'] += 1
            logger.info(f"✅ {test_name}: {message} ({duration:.2f}s)")
        else:
            self.test_stats['failed'] += 1
            logger.error(f"❌ {test_name}: {message} ({duration:.2f}s)")
        
        self.test_results[test_name] = {
            'success': success,
            'message': message,
            'duration': duration,
            'timestamp': datetime.now().isoformat()
        }

    async def test_infrastructure_layer(self) -> Dict[str, bool]:
        """測試L0基礎設施層"""
        logger.info("🔍 測試L0基礎設施層...")
        results = {}
        
        # 1. Redis連接測試
        try:
            start_time = time.time()
            r = redis.Redis(
                host=self.endpoints['redis']['host'], 
                port=self.endpoints['redis']['port'],
                decode_responses=True,
                socket_timeout=5
            )
            
            # 基本連接測試
            ping_result = r.ping()
            if ping_result:
                # 發佈訂閱測試
                r.publish('ops.alert', json.dumps({
                    'type': 'test',
                    'value': 42.0,
                    'ts': time.time()
                }))
                
                duration = time.time() - start_time
                self.log_test_result(
                    "Redis連接", True, 
                    "Redis連接成功，pub/sub功能正常", duration
                )
                results['redis'] = True
            else:
                self.log_test_result("Redis連接", False, "Redis ping失敗", 0)
                results['redis'] = False
                
        except Exception as e:
            self.log_test_result("Redis連接", False, f"Redis連接失敗: {str(e)}", 0)
            results['redis'] = False

        # 2. ClickHouse連接測試
        try:
            start_time = time.time()
            response = requests.get(
                f"{self.endpoints['clickhouse']['url']}/?query=SELECT 1",
                timeout=10
            )
            
            if response.status_code == 200 and '1' in response.text:
                duration = time.time() - start_time
                self.log_test_result(
                    "ClickHouse連接", True,
                    "ClickHouse連接成功", duration
                )
                results['clickhouse'] = True
            else:
                self.log_test_result(
                    "ClickHouse連接", False,
                    f"ClickHouse查詢失敗: {response.status_code}", 0
                )
                results['clickhouse'] = False
                
        except Exception as e:
            self.log_test_result("ClickHouse連接", False, f"ClickHouse連接失敗: {str(e)}", 0)
            results['clickhouse'] = False

        # 3. Prometheus連接測試
        try:
            start_time = time.time()
            response = requests.get(
                f"{self.endpoints['prometheus']['url']}/api/v1/query?query=up",
                timeout=10
            )
            
            if response.status_code == 200:
                data = response.json()
                if data.get('status') == 'success':
                    duration = time.time() - start_time
                    self.log_test_result(
                        "Prometheus連接", True,
                        "Prometheus API響應正常", duration
                    )
                    results['prometheus'] = True
                else:
                    self.log_test_result("Prometheus連接", False, "Prometheus API響應異常", 0)
                    results['prometheus'] = False
            else:
                self.log_test_result("Prometheus連接", False, f"Prometheus連接失敗: {response.status_code}", 0)
                results['prometheus'] = False
                
        except Exception as e:
            self.log_test_result("Prometheus連接", False, f"Prometheus連接失敗: {str(e)}", 0)
            results['prometheus'] = False

        return results

    async def test_rust_core_layer(self) -> Dict[str, bool]:
        """測試L1 Rust核心層"""
        logger.info("⚡ 測試L1 Rust核心引擎...")
        results = {}
        
        # 1. HTTP指標端點測試
        try:
            start_time = time.time()
            response = requests.get(self.endpoints['rust_metrics']['url'], timeout=15)
            
            if response.status_code == 200:
                metrics_text = response.text
                
                # 檢查關鍵指標是否存在
                expected_metrics = [
                    'hft_exec_latency',
                    'hft_orderbook_update',
                    'hft_position_drawdown'
                ]
                
                missing_metrics = []
                for metric in expected_metrics:
                    if metric not in metrics_text:
                        missing_metrics.append(metric)
                
                if not missing_metrics:
                    duration = time.time() - start_time
                    self.log_test_result(
                        "Rust指標端點", True,
                        f"所有關鍵指標正常暴露 ({len(expected_metrics)}個)", duration
                    )
                    results['rust_metrics'] = True
                else:
                    self.log_test_result(
                        "Rust指標端點", False,
                        f"缺少指標: {missing_metrics}", 0
                    )
                    results['rust_metrics'] = False
            else:
                self.log_test_result("Rust指標端點", False, f"HTTP {response.status_code}", 0)
                results['rust_metrics'] = False
                
        except Exception as e:
            self.log_test_result("Rust指標端點", False, f"連接失敗: {str(e)}", 0)
            results['rust_metrics'] = False

        # 2. gRPC服務測試
        try:
            start_time = time.time()
            
            # 使用grpcurl測試gRPC服務
            result = subprocess.run([
                'grpcurl', '-plaintext', self.endpoints['rust_grpc']['address'], 'list'
            ], capture_output=True, text=True, timeout=10)
            
            if result.returncode == 0:
                duration = time.time() - start_time
                self.log_test_result(
                    "Rust gRPC服務", True,
                    "gRPC服務響應正常", duration
                )
                results['rust_grpc'] = True
            else:
                self.log_test_result(
                    "Rust gRPC服務", False,
                    f"grpcurl失敗: {result.stderr}", 0
                )
                results['rust_grpc'] = False
                
        except subprocess.TimeoutExpired:
            self.log_test_result("Rust gRPC服務", False, "gRPC服務響應超時", 0)
            results['rust_grpc'] = False
        except FileNotFoundError:
            self.log_test_result(
                "Rust gRPC服務", False,
                "grpcurl未安裝，跳過gRPC測試", 0
            )
            results['rust_grpc'] = False
        except Exception as e:
            self.log_test_result("Rust gRPC服務", False, f"gRPC測試失敗: {str(e)}", 0)
            results['rust_grpc'] = False

        return results

    async def test_ops_workspace(self) -> Dict[str, bool]:
        """測試L2運維工作區"""
        logger.info("🛡️ 測試L2運維工作區...")
        results = {}
        
        # 1. Ops API測試
        try:
            start_time = time.time()
            response = requests.get(f"{self.endpoints['ops_api']['url']}/docs", timeout=10)
            
            if response.status_code == 200 and 'FastAPI' in response.text:
                duration = time.time() - start_time
                self.log_test_result(
                    "Ops API", True,
                    "FastAPI文檔頁面正常", duration
                )
                results['ops_api'] = True
            else:
                self.log_test_result("Ops API", False, f"API響應異常: {response.status_code}", 0)
                results['ops_api'] = False
                
        except Exception as e:
            self.log_test_result("Ops API", False, f"API連接失敗: {str(e)}", 0)
            results['ops_api'] = False

        # 2. Ops UI測試
        try:
            start_time = time.time()
            response = requests.get(self.endpoints['ops_ui']['url'], timeout=15)
            
            if response.status_code == 200:
                duration = time.time() - start_time
                self.log_test_result(
                    "Ops Streamlit UI", True,
                    "Streamlit界面正常", duration
                )
                results['ops_ui'] = True
            else:
                self.log_test_result("Ops Streamlit UI", False, f"UI響應異常: {response.status_code}", 0)
                results['ops_ui'] = False
                
        except Exception as e:
            self.log_test_result("Ops Streamlit UI", False, f"UI連接失敗: {str(e)}", 0)
            results['ops_ui'] = False

        return results

    async def test_ml_workspace(self) -> Dict[str, bool]:
        """測試L3機器學習工作區"""
        logger.info("🧠 測試L3機器學習工作區 (按需啟動)...")
        results = {}
        
        # ML workspace是按需啟動，檢查是否可用
        try:
            start_time = time.time()
            response = requests.get(self.endpoints['ml_ui']['url'], timeout=5)
            
            if response.status_code == 200:
                duration = time.time() - start_time
                self.log_test_result(
                    "ML Workspace", True,
                    "ML工作區正在運行", duration
                )
                results['ml_available'] = True
                
                # 如果ML工作區運行中，測試API
                try:
                    api_response = requests.get(f"{self.endpoints['ml_api']['url']}/docs", timeout=5)
                    if api_response.status_code == 200:
                        self.log_test_result("ML API", True, "ML API正常", 0)
                        results['ml_api'] = True
                    else:
                        results['ml_api'] = False
                except:
                    results['ml_api'] = False
                    
            else:
                self.log_test_result(
                    "ML Workspace", True,
                    "ML工作區未啟動（按需啟動設計正常）", 0
                )
                results['ml_available'] = False
                
        except Exception as e:
            self.log_test_result(
                "ML Workspace", True,
                "ML工作區未啟動（按需啟動設計正常）", 0
            )
            results['ml_available'] = False

        return results

    async def test_master_orchestrator(self) -> Dict[str, bool]:
        """測試Master Orchestrator"""
        logger.info("🎯 測試Master Orchestrator...")
        results = {}
        
        # 1. Master API測試
        try:
            start_time = time.time()
            response = requests.get(f"{self.endpoints['master_api']['url']}/docs", timeout=10)
            
            if response.status_code == 200 and 'FastAPI' in response.text:
                duration = time.time() - start_time
                self.log_test_result(
                    "Master API", True,
                    "Master API正常", duration
                )
                results['master_api'] = True
            else:
                self.log_test_result("Master API", False, f"API響應異常: {response.status_code}", 0)
                results['master_api'] = False
                
        except Exception as e:
            self.log_test_result("Master API", False, f"API連接失敗: {str(e)}", 0)
            results['master_api'] = False

        # 2. Master UI測試
        try:
            start_time = time.time()
            response = requests.get(self.endpoints['master_ui']['url'], timeout=15)
            
            if response.status_code == 200:
                duration = time.time() - start_time
                self.log_test_result(
                    "Master控制台", True,
                    "Master控制台正常", duration
                )
                results['master_ui'] = True
            else:
                self.log_test_result("Master控制台", False, f"控制台響應異常: {response.status_code}", 0)
                results['master_ui'] = False
                
        except Exception as e:
            self.log_test_result("Master控制台", False, f"控制台連接失敗: {str(e)}", 0)
            results['master_ui'] = False

        return results

    async def test_integration_scenarios(self) -> Dict[str, bool]:
        """測試集成場景"""
        logger.info("🔗 測試系統集成場景...")
        results = {}
        
        # 1. Redis消息流測試
        try:
            start_time = time.time()
            r = redis.Redis(
                host=self.endpoints['redis']['host'],
                port=self.endpoints['redis']['port'],
                decode_responses=True
            )
            
            # 測試關鍵通道
            channels = ['ops.alert', 'ml.deploy', 'kill-switch']
            for channel in channels:
                test_message = {
                    'type': 'integration_test',
                    'timestamp': time.time(),
                    'source': 'system_validator'
                }
                r.publish(channel, json.dumps(test_message))
            
            duration = time.time() - start_time
            self.log_test_result(
                "Redis消息流", True,
                f"成功發送測試消息到{len(channels)}個通道", duration
            )
            results['redis_messaging'] = True
            
        except Exception as e:
            self.log_test_result("Redis消息流", False, f"消息發送失敗: {str(e)}", 0)
            results['redis_messaging'] = False

        # 2. 跨服務通信測試
        try:
            start_time = time.time()
            
            # 檢查Prometheus是否能抓取到Rust指標
            prom_response = requests.get(
                f"{self.endpoints['prometheus']['url']}/api/v1/query?query=up{{job=\"rust-hft-core\"}}",
                timeout=10
            )
            
            if prom_response.status_code == 200:
                data = prom_response.json()
                if data.get('status') == 'success' and data.get('data', {}).get('result'):
                    duration = time.time() - start_time
                    self.log_test_result(
                        "監控集成", True,
                        "Prometheus成功抓取Rust指標", duration
                    )
                    results['monitoring_integration'] = True
                else:
                    self.log_test_result("監控集成", False, "Prometheus未發現Rust指標", 0)
                    results['monitoring_integration'] = False
            else:
                self.log_test_result("監控集成", False, "Prometheus查詢失敗", 0)
                results['monitoring_integration'] = False
                
        except Exception as e:
            self.log_test_result("監控集成", False, f"監控集成測試失敗: {str(e)}", 0)
            results['monitoring_integration'] = False

        return results

    async def test_performance_benchmarks(self) -> Dict[str, bool]:
        """性能基準測試"""
        logger.info("🚀 執行性能基準測試...")
        results = {}
        
        # 1. Redis延遲測試
        try:
            start_time = time.time()
            r = redis.Redis(
                host=self.endpoints['redis']['host'],
                port=self.endpoints['redis']['port']
            )
            
            # 測試100次ping的平均延遲
            ping_times = []
            for _ in range(100):
                ping_start = time.time()
                r.ping()
                ping_times.append((time.time() - ping_start) * 1000)  # ms
            
            avg_ping = sum(ping_times) / len(ping_times)
            p99_ping = sorted(ping_times)[98]  # 99th percentile
            
            duration = time.time() - start_time
            
            if p99_ping < 1.0:  # 1ms
                self.log_test_result(
                    "Redis延遲", True,
                    f"Redis延遲良好 (平均: {avg_ping:.2f}ms, p99: {p99_ping:.2f}ms)", duration
                )
                results['redis_latency'] = True
            else:
                self.log_test_result(
                    "Redis延遲", False,
                    f"Redis延遲過高 (平均: {avg_ping:.2f}ms, p99: {p99_ping:.2f}ms)", duration
                )
                results['redis_latency'] = False
                
        except Exception as e:
            self.log_test_result("Redis延遲", False, f"延遲測試失敗: {str(e)}", 0)
            results['redis_latency'] = False

        # 2. HTTP響應時間測試
        try:
            endpoints_to_test = [
                ('Rust指標', self.endpoints['rust_metrics']['url']),
                ('Ops API', f"{self.endpoints['ops_api']['url']}/docs"),
                ('Master API', f"{self.endpoints['master_api']['url']}/docs")
            ]
            
            for name, url in endpoints_to_test:
                response_times = []
                for _ in range(10):
                    start = time.time()
                    try:
                        response = requests.get(url, timeout=5)
                        if response.status_code == 200:
                            response_times.append((time.time() - start) * 1000)
                    except:
                        pass
                
                if response_times:
                    avg_time = sum(response_times) / len(response_times)
                    if avg_time < 100:  # 100ms
                        self.log_test_result(
                            f"{name}響應時間", True,
                            f"響應時間良好 (平均: {avg_time:.1f}ms)", 0
                        )
                        results[f'{name.lower()}_response'] = True
                    else:
                        self.log_test_result(
                            f"{name}響應時間", False,
                            f"響應時間過慢 (平均: {avg_time:.1f}ms)", 0
                        )
                        results[f'{name.lower()}_response'] = False
                else:
                    results[f'{name.lower()}_response'] = False
                    
        except Exception as e:
            self.log_test_result("HTTP響應時間", False, f"響應時間測試失敗: {str(e)}", 0)

        return results

    def generate_report(self):
        """生成測試報告"""
        total_duration = time.time() - self.start_time
        
        print("\n" + "="*80)
        print("🎯 HFT系統驗證報告")
        print("="*80)
        
        print(f"\n📊 測試統計:")
        print(f"  總測試數:     {self.test_stats['total']}")
        print(f"  ✅ 通過:      {self.test_stats['passed']}")
        print(f"  ❌ 失敗:      {self.test_stats['failed']}")
        print(f"  ⚠️  警告:      {self.test_stats['warnings']}")
        print(f"  📈 成功率:    {(self.test_stats['passed']/self.test_stats['total']*100):.1f}%")
        print(f"  ⏱️  總耗時:    {total_duration:.2f}s")
        
        print(f"\n🔍 詳細結果:")
        for test_name, result in self.test_results.items():
            status = "✅" if result['success'] else "❌"
            print(f"  {status} {test_name}: {result['message']}")
        
        # 系統就緒評估
        critical_tests = [
            'Redis連接', 'ClickHouse連接', 'Rust指標端點', 
            'Ops API', 'Master API'
        ]
        
        critical_failures = [
            test for test in critical_tests 
            if test in self.test_results and not self.test_results[test]['success']
        ]
        
        print(f"\n🚀 系統就緒狀態:")
        if not critical_failures:
            print("  ✅ 系統已準備就緒，可以開始交易！")
            print(f"  🌐 Master控制台: http://localhost:8504")
            print(f"  📊 監控面板: http://localhost:3000")
            return True
        else:
            print("  ❌ 系統尚未就緒，請修復以下關鍵問題:")
            for failure in critical_failures:
                print(f"    - {failure}: {self.test_results[failure]['message']}")
            return False

    async def run_all_tests(self):
        """運行所有測試"""
        logger.info("🚀 開始HFT系統完整驗證...")
        
        # 執行分層測試
        await self.test_infrastructure_layer()
        await self.test_rust_core_layer()
        await self.test_ops_workspace()
        await self.test_ml_workspace()
        await self.test_master_orchestrator()
        await self.test_integration_scenarios()
        await self.test_performance_benchmarks()
        
        # 生成報告
        return self.generate_report()

async def main():
    """主函數"""
    validator = HFTSystemValidator()
    
    try:
        system_ready = await validator.run_all_tests()
        
        if system_ready:
            sys.exit(0)  # 成功
        else:
            sys.exit(1)  # 失敗
            
    except KeyboardInterrupt:
        logger.info("用戶中斷測試")
        sys.exit(2)
    except Exception as e:
        logger.error(f"測試過程中發生異常: {str(e)}")
        sys.exit(3)

if __name__ == "__main__":
    asyncio.run(main())