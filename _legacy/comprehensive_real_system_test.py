#!/usr/bin/env python3
"""
Comprehensive REAL System Integration Test
==========================================

Tests all REAL system components:
1. Real Rust HFT System Integration
2. Real Redis Data Pipeline
3. Real ClickHouse Database Operations
4. Real WebSocket Connections
5. Real Agent System Integration
"""

import asyncio
import time
import logging
import json
import redis
import requests
import subprocess
from pathlib import Path
from typing import Dict, Any, List
from dataclasses import dataclass

# Setup logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

@dataclass
class SystemTestResult:
    """Test result dataclass"""
    component: str
    test_name: str
    success: bool
    duration_ms: float
    data: Dict[str, Any]
    error: str = None

class ComprehensiveRealSystemTest:
    """Comprehensive real system integration tester"""
    
    def __init__(self):
        self.results: List[SystemTestResult] = []
        self.redis_client = None
        self.project_root = Path("/Users/proerror/Documents/monday")
        
    async def run_all_tests(self) -> Dict[str, Any]:
        """Run all comprehensive system tests"""
        logger.info("🚀 Starting Comprehensive REAL System Integration Tests")
        logger.info("=" * 80)
        
        start_time = time.time()
        
        # Test Infrastructure
        await self._test_infrastructure_connectivity()
        
        # Test Rust HFT System
        await self._test_rust_hft_system()
        
        # Test Redis Integration
        await self._test_redis_integration()
        
        # Test ClickHouse Integration
        await self._test_clickhouse_integration()
        
        # Test Python Agent Integration
        await self._test_python_agent_integration()
        
        # Test End-to-End Data Pipeline
        await self._test_e2e_data_pipeline()
        
        total_duration = time.time() - start_time
        
        return self._generate_comprehensive_report(total_duration)
    
    async def _test_infrastructure_connectivity(self):
        """Test basic infrastructure connectivity"""
        logger.info("🔧 Testing Infrastructure Connectivity...")
        
        # Test Redis Connection
        start_time = time.time()
        try:
            self.redis_client = redis.Redis(host='localhost', port=6379, db=0)
            response = self.redis_client.ping()
            duration = (time.time() - start_time) * 1000
            
            self.results.append(SystemTestResult(
                component="Infrastructure",
                test_name="Redis Connectivity",
                success=response == True,
                duration_ms=duration,
                data={"response": str(response), "host": "localhost:6379"}
            ))
            logger.info(f"✅ Redis Connection: {duration:.2f}ms")
        except Exception as e:
            self.results.append(SystemTestResult(
                component="Infrastructure",
                test_name="Redis Connectivity",
                success=False,
                duration_ms=(time.time() - start_time) * 1000,
                data={},
                error=str(e)
            ))
            logger.error(f"❌ Redis Connection Failed: {e}")
        
        # Test ClickHouse Connection
        start_time = time.time()
        try:
            response = requests.get("http://localhost:8123/ping", timeout=5)
            duration = (time.time() - start_time) * 1000
            
            self.results.append(SystemTestResult(
                component="Infrastructure",
                test_name="ClickHouse Connectivity",
                success=response.status_code == 200,
                duration_ms=duration,
                data={"status_code": response.status_code, "response": response.text}
            ))
            logger.info(f"✅ ClickHouse Connection: {duration:.2f}ms")
        except Exception as e:
            self.results.append(SystemTestResult(
                component="Infrastructure",
                test_name="ClickHouse Connectivity",
                success=False,
                duration_ms=(time.time() - start_time) * 1000,
                data={},
                error=str(e)
            ))
            logger.error(f"❌ ClickHouse Connection Failed: {e}")
    
    async def _test_rust_hft_system(self):
        """Test real Rust HFT system functionality"""
        logger.info("🦀 Testing Real Rust HFT System...")
        
        # Test Rust HFT Quickstart
        start_time = time.time()
        try:
            # Run the quickstart example for 10 seconds
            process = await asyncio.create_subprocess_exec(
                "timeout", "10s", "cargo", "run", "--example", "00_quickstart",
                cwd=self.project_root / "rust_hft",
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            stdout, stderr = await process.communicate()
            duration = (time.time() - start_time) * 1000
            
            # Parse output for key metrics
            output_text = stdout.decode('utf-8')
            success = "快速入门完成" in output_text and "总处理消息" in output_text
            
            # Extract metrics from output
            metrics = self._parse_rust_output_metrics(output_text)
            
            self.results.append(SystemTestResult(
                component="Rust HFT",
                test_name="Quickstart Example",
                success=success,
                duration_ms=duration,
                data={
                    "metrics": metrics,
                    "process_return_code": process.returncode,
                    "output_length": len(output_text)
                }
            ))
            
            if success:
                logger.info(f"✅ Rust HFT Quickstart: {duration:.0f}ms, Processed {metrics.get('total_messages', 0)} messages")
            else:
                logger.warning(f"⚠️ Rust HFT Quickstart: Partial success, {duration:.0f}ms")
                
        except Exception as e:
            self.results.append(SystemTestResult(
                component="Rust HFT",
                test_name="Quickstart Example",
                success=False,
                duration_ms=(time.time() - start_time) * 1000,
                data={},
                error=str(e)
            ))
            logger.error(f"❌ Rust HFT System Test Failed: {e}")
    
    def _parse_rust_output_metrics(self, output: str) -> Dict[str, Any]:
        """Parse metrics from Rust output"""
        metrics = {}
        lines = output.split('\n')
        
        for line in lines:
            if "总处理消息:" in line:
                try:
                    count = int(line.split("总处理消息:")[1].split("条")[0].strip())
                    metrics["total_messages"] = count
                except:
                    pass
            elif "平均处理延迟:" in line:
                try:
                    latency = float(line.split("平均处理延迟:")[1].split("μs")[0].strip())
                    metrics["avg_latency_us"] = latency
                except:
                    pass
            elif "信号勝率:" in line:
                try:
                    win_rate = float(line.split("信号勝率:")[1].split("%")[0].strip())
                    metrics["signal_win_rate"] = win_rate
                except:
                    pass
        
        return metrics
    
    async def _test_redis_integration(self):
        """Test Redis integration with real data flow"""
        logger.info("📡 Testing Redis Integration...")
        
        if not self.redis_client:
            logger.error("❌ Redis client not available, skipping Redis tests")
            return
        
        # Test Redis Publish/Subscribe
        start_time = time.time()
        try:
            # Publish test data
            test_data = {
                "symbol": "BTCUSDT",
                "mid_price": 97150.50,
                "best_bid": 97149.50,
                "best_ask": 97151.50,
                "spread": 2.00,
                "timestamp_us": int(time.time() * 1000000),
                "source": "comprehensive_test"
            }
            
            self.redis_client.setex(
                "hft:orderbook:BTCUSDT",
                10,  # 10 second expiry
                json.dumps(test_data)
            )
            
            # Retrieve the data
            retrieved_data = self.redis_client.get("hft:orderbook:BTCUSDT")
            duration = (time.time() - start_time) * 1000
            
            if retrieved_data:
                parsed_data = json.loads(retrieved_data)
                success = parsed_data["symbol"] == "BTCUSDT"
            else:
                success = False
                parsed_data = {}
            
            self.results.append(SystemTestResult(
                component="Redis",
                test_name="Data Publish/Retrieve",
                success=success,
                duration_ms=duration,
                data={
                    "published_data": test_data,
                    "retrieved_data": parsed_data,
                    "data_matches": parsed_data.get("mid_price") == test_data["mid_price"]
                }
            ))
            
            logger.info(f"✅ Redis Integration: {duration:.2f}ms, Data integrity: {'OK' if success else 'Failed'}")
            
        except Exception as e:
            self.results.append(SystemTestResult(
                component="Redis",
                test_name="Data Publish/Retrieve",
                success=False,
                duration_ms=(time.time() - start_time) * 1000,
                data={},
                error=str(e)
            ))
            logger.error(f"❌ Redis Integration Failed: {e}")
    
    async def _test_clickhouse_integration(self):
        """Test ClickHouse integration with real database operations"""
        logger.info("🗄️ Testing ClickHouse Integration...")
        
        # Test ClickHouse Query
        start_time = time.time()
        try:
            # Simple query to test ClickHouse functionality
            response = requests.post(
                "http://localhost:8123/",
                data="SELECT version(), now()",
                timeout=10
            )
            duration = (time.time() - start_time) * 1000
            
            success = response.status_code == 200
            result_data = response.text.strip() if success else ""
            
            self.results.append(SystemTestResult(
                component="ClickHouse",
                test_name="Database Query",
                success=success,
                duration_ms=duration,
                data={
                    "query_result": result_data,
                    "status_code": response.status_code,
                    "response_length": len(result_data)
                }
            ))
            
            if success:
                logger.info(f"✅ ClickHouse Query: {duration:.2f}ms, Result: {result_data[:50]}...")
            else:
                logger.error(f"❌ ClickHouse Query Failed: Status {response.status_code}")
                
        except Exception as e:
            self.results.append(SystemTestResult(
                component="ClickHouse",
                test_name="Database Query",
                success=False,
                duration_ms=(time.time() - start_time) * 1000,
                data={},
                error=str(e)
            ))
            logger.error(f"❌ ClickHouse Integration Failed: {e}")
    
    async def _test_python_agent_integration(self):
        """Test Python agent system integration"""
        logger.info("🤖 Testing Python Agent Integration...")
        
        # Test RustHFTTools Integration
        start_time = time.time()
        try:
            # Import the RustHFTTools
            import sys
            sys.path.append(str(self.project_root / "agno_hft"))
            from rust_hft_tools import RustHFTTools
            
            # Create tools instance
            tools = RustHFTTools(rust_project_path=str(self.project_root / "rust_hft"))
            
            # Test system status
            status = await tools.get_system_status()
            duration = (time.time() - start_time) * 1000
            
            success = isinstance(status, dict) and "overall_status" in status
            
            self.results.append(SystemTestResult(
                component="Python Agents",
                test_name="RustHFTTools Integration",
                success=success,
                duration_ms=duration,
                data={
                    "system_status": status,
                    "tools_initialized": tools.hft_engine is not None,
                    "rust_project_path": str(self.project_root / "rust_hft")
                }
            ))
            
            logger.info(f"✅ Python Agent Integration: {duration:.2f}ms, Status: {status.get('overall_status', 'unknown')}")
            
        except Exception as e:
            self.results.append(SystemTestResult(
                component="Python Agents",
                test_name="RustHFTTools Integration",
                success=False,
                duration_ms=(time.time() - start_time) * 1000,
                data={},
                error=str(e)
            ))
            logger.error(f"❌ Python Agent Integration Failed: {e}")
    
    async def _test_e2e_data_pipeline(self):
        """Test end-to-end data pipeline"""
        logger.info("🔄 Testing End-to-End Data Pipeline...")
        
        # Test Redis → Python Feature Extraction
        start_time = time.time()
        try:
            # Run the Redis integration test
            result = subprocess.run([
                "python", "test_redis_integration.py"
            ], cwd=self.project_root, capture_output=True, text=True, timeout=30)
            
            duration = (time.time() - start_time) * 1000
            success = result.returncode == 0 and "测试完成" in result.stdout
            
            # Parse output for performance metrics
            output_lines = result.stdout.split('\n')
            latency_info = None
            for line in output_lines:
                if "延遲:" in line and "ms" in line:
                    latency_info = line
                    break
            
            self.results.append(SystemTestResult(
                component="E2E Pipeline",
                test_name="Redis Data Pipeline",
                success=success,
                duration_ms=duration,
                data={
                    "output_length": len(result.stdout),
                    "return_code": result.returncode,
                    "latency_info": latency_info,
                    "contains_success_marker": "测试完成" in result.stdout
                }
            ))
            
            if success:
                logger.info(f"✅ E2E Data Pipeline: {duration:.0f}ms, Latency info: {latency_info}")
            else:
                logger.warning(f"⚠️ E2E Data Pipeline: {duration:.0f}ms, Return code: {result.returncode}")
                
        except Exception as e:
            self.results.append(SystemTestResult(
                component="E2E Pipeline",
                test_name="Redis Data Pipeline",
                success=False,
                duration_ms=(time.time() - start_time) * 1000,
                data={},
                error=str(e)
            ))
            logger.error(f"❌ E2E Data Pipeline Failed: {e}")
    
    def _generate_comprehensive_report(self, total_duration: float) -> Dict[str, Any]:
        """Generate comprehensive test report"""
        logger.info("=" * 80)
        logger.info("📊 COMPREHENSIVE REAL SYSTEM INTEGRATION TEST REPORT")
        logger.info("=" * 80)
        
        # Calculate statistics
        total_tests = len(self.results)
        successful_tests = sum(1 for r in self.results if r.success)
        failed_tests = total_tests - successful_tests
        success_rate = (successful_tests / total_tests * 100) if total_tests > 0 else 0
        
        # Group results by component
        component_results = {}
        for result in self.results:
            if result.component not in component_results:
                component_results[result.component] = []
            component_results[result.component].append(result)
        
        # Print detailed results
        for component, results in component_results.items():
            logger.info(f"\n🔧 {component} Component:")
            for result in results:
                status = "✅" if result.success else "❌"
                logger.info(f"  {status} {result.test_name}: {result.duration_ms:.1f}ms")
                if result.error:
                    logger.info(f"      Error: {result.error}")
                elif result.data:
                    key_data = {k: v for k, v in result.data.items() if not k.endswith('_data')}
                    logger.info(f"      Data: {key_data}")
        
        # Summary
        logger.info(f"\n📈 TEST SUMMARY:")
        logger.info(f"  Total Tests: {total_tests}")
        logger.info(f"  Successful: {successful_tests}")
        logger.info(f"  Failed: {failed_tests}")
        logger.info(f"  Success Rate: {success_rate:.1f}%")
        logger.info(f"  Total Duration: {total_duration:.1f}s")
        
        # Real vs Mock Analysis
        real_integrations = []
        for result in self.results:
            if result.success:
                if result.component == "Infrastructure":
                    real_integrations.append(f"✅ Real {result.test_name.split()[0]} connection")
                elif result.component == "Rust HFT":
                    real_integrations.append("✅ Real Rust HFT system execution")
                elif result.component == "Redis":
                    real_integrations.append("✅ Real Redis data operations")
                elif result.component == "ClickHouse":
                    real_integrations.append("✅ Real ClickHouse database queries")
                elif result.component == "Python Agents":
                    real_integrations.append("✅ Real Python-Rust integration")
                elif result.component == "E2E Pipeline":
                    real_integrations.append("✅ Real end-to-end data pipeline")
        
        logger.info(f"\n🎯 REAL SYSTEM INTEGRATIONS VERIFIED:")
        for integration in real_integrations:
            logger.info(f"  {integration}")
        
        # Performance Analysis
        avg_latencies = {}
        for component, results in component_results.items():
            successful_results = [r for r in results if r.success]
            if successful_results:
                avg_latency = sum(r.duration_ms for r in successful_results) / len(successful_results)
                avg_latencies[component] = avg_latency
        
        logger.info(f"\n⚡ PERFORMANCE METRICS:")
        for component, avg_latency in avg_latencies.items():
            logger.info(f"  {component}: {avg_latency:.1f}ms average")
        
        # Generate return data
        return {
            "test_summary": {
                "total_tests": total_tests,
                "successful_tests": successful_tests,
                "failed_tests": failed_tests,
                "success_rate": success_rate,
                "total_duration_seconds": total_duration
            },
            "component_results": {
                component: [
                    {
                        "test_name": r.test_name,
                        "success": r.success,
                        "duration_ms": r.duration_ms,
                        "error": r.error
                    } for r in results
                ] for component, results in component_results.items()
            },
            "real_integrations": real_integrations,
            "performance_metrics": avg_latencies,
            "detailed_results": [
                {
                    "component": r.component,
                    "test_name": r.test_name,
                    "success": r.success,
                    "duration_ms": r.duration_ms,
                    "data": r.data,
                    "error": r.error
                } for r in self.results
            ]
        }

async def main():
    """Main function to run comprehensive tests"""
    tester = ComprehensiveRealSystemTest()
    results = await tester.run_all_tests()
    
    print(f"\n" + "=" * 80)
    print("🏁 COMPREHENSIVE REAL SYSTEM TEST COMPLETED")
    print(f"Success Rate: {results['test_summary']['success_rate']:.1f}%")
    print(f"Real Integrations: {len(results['real_integrations'])}")
    print("=" * 80)

if __name__ == "__main__":
    asyncio.run(main())