#!/usr/bin/env python3
"""
HFT 系統命令行接口
=================

統一的命令行工具管理 HFT 系統：
- 系統初始化和配置
- 交易策略管理
- 實時監控和狀態查詢
- 參數優化和調整
- 緊急控制和恢復
"""

import asyncio
import click
import json
import sys
import os
from datetime import datetime
from typing import Optional, List
import logging
from rich.console import Console
from rich.table import Table
from rich.panel import Panel
from rich.progress import Progress, SpinnerColumn, TextColumn
from rich.live import Live
from rich import print as rprint

# 添加項目路徑
sys.path.append(os.path.dirname(os.path.abspath(__file__)))

from system_integration import HFTSystemIntegration, SystemConfiguration, SystemMode
from workflows.workflow_manager_v2 import workflow_manager_v2

console = Console()

# 全局系統實例
hft_system = None


def load_system_config(config_path: str) -> SystemConfiguration:
    """加載系統配置"""
    try:
        if os.path.exists(config_path):
            return SystemConfiguration.load_from_file(config_path)
        else:
            # 使用默認配置
            return SystemConfiguration(
                mode=SystemMode.DEVELOPMENT,
                symbols=["BTCUSDT"],
                risk_limits={"max_exposure": 50000, "max_var": -2500},
                model_config={"hidden_size": 128},
                rust_hft_config={"max_latency_us": 1000},
                monitoring_config={"health_check_interval": 60}
            )
    except Exception as e:
        console.print(f"[red]❌ 配置加載失敗: {e}[/red]")
        sys.exit(1)


@click.group()
@click.option('--config', '-c', default='config/system_config.json', 
              help='系統配置文件路徑')
@click.option('--verbose', '-v', is_flag=True, help='詳細輸出')
@click.pass_context
def cli(ctx, config, verbose):
    """HFT 高頻交易系統管理工具"""
    ctx.ensure_object(dict)
    
    # 設置日誌級別
    log_level = logging.DEBUG if verbose else logging.INFO
    logging.basicConfig(level=log_level)
    
    # 加載系統配置
    ctx.obj['config'] = load_system_config(config)
    ctx.obj['verbose'] = verbose
    
    # 初始化系統
    global hft_system
    hft_system = HFTSystemIntegration(ctx.obj['config'])


@cli.command()
@click.pass_context
def init(ctx):
    """初始化 HFT 系統"""
    async def _init():
        config = ctx.obj['config']
        
        with console.status("[bold green]正在初始化 HFT 系統..."):
            result = await hft_system.initialize_system()
        
        if result['success']:
            console.print("\n[green]✅ 系統初始化成功![/green]")
            
            # 顯示初始化結果
            table = Table(title="系統初始化結果")
            table.add_column("項目", style="cyan")
            table.add_column("狀態", style="green")
            table.add_column("詳情")
            
            table.add_row("系統模式", config.mode.value, f"配置: {len(config.symbols)} 交易對")
            table.add_row("系統狀態", result['system_state'], "就緒")
            table.add_row("交易對", str(result['initialized_symbols']), "已初始化")
            table.add_row("健康檢查", "✅ 通過" if result['health_status']['passed'] else "❌ 失敗", 
                         f"評分: {result['health_status'].get('health_score', 'N/A')}")
            table.add_row("Rust 引擎", "✅ 已連接" if result['rust_status']['success'] else "❌ 失敗",
                         f"延遲: {result['rust_status'].get('latency_us', 'N/A')}μs")
            
            console.print(table)
        else:
            console.print(f"[red]❌ 系統初始化失敗: {result['error']}[/red]")
            sys.exit(1)
    
    asyncio.run(_init())


@cli.command()
@click.option('--symbols', '-s', help='指定交易對 (逗號分隔)')
@click.option('--mode', '-m', type=click.Choice(['gradual', 'immediate']), 
              default='gradual', help='啟動模式')
@click.pass_context
def start(ctx, symbols, mode):
    """啟動交易"""
    async def _start():
        symbol_list = symbols.split(',') if symbols else None
        
        with console.status(f"[bold yellow]正在啟動交易 ({mode} 模式)..."):
            result = await hft_system.start_trading(symbol_list, mode)
        
        if result['success']:
            console.print(f"\n[green]✅ 交易啟動成功![/green]")
            
            table = Table(title="交易啟動結果")
            table.add_column("交易對", style="cyan")
            table.add_column("工作流ID", style="yellow")
            table.add_column("模式", style="green")
            table.add_column("狀態")
            
            for symbol, info in result['started_symbols'].items():
                table.add_row(
                    symbol,
                    info['workflow_id'][:8] + "...",
                    info['mode'],
                    "✅ 已啟動" if info['success'] else "❌ 失敗"
                )
            
            console.print(table)
            console.print(f"\n📊 總計活躍策略: {result['total_active']}")
        else:
            console.print(f"[red]❌ 交易啟動失敗: {result['error']}[/red]")
    
    asyncio.run(_start())


@cli.command()
@click.option('--reason', '-r', default='手動觸發', help='停止原因')
@click.pass_context
def stop(ctx, reason):
    """緊急停止所有交易"""
    async def _stop():
        with console.status("[bold red]正在執行緊急停止..."):
            result = await hft_system.emergency_stop(reason)
        
        if result['success']:
            console.print(f"\n[red]🛑 緊急停止執行完成![/red]")
            
            event = result['emergency_event']
            console.print(f"停止時間: {event['timestamp']}")
            console.print(f"停止原因: {event['reason']}")
            console.print(f"影響策略: {result['stopped_strategies']}")
            console.print(f"系統狀態: {result['system_state']}")
        else:
            console.print(f"[red]❌ 緊急停止失敗: {result['error']}[/red]")
    
    asyncio.run(_stop())


@cli.command()
@click.option('--symbols', '-s', help='指定要優化的交易對 (逗號分隔)')
@click.option('--target', '-t', default='sharpe_ratio', 
              type=click.Choice(['sharpe_ratio', 'max_drawdown', 'win_rate']),
              help='優化目標')
@click.pass_context
def optimize(ctx, symbols, target):
    """優化策略參數"""
    async def _optimize():
        symbol_list = symbols.split(',') if symbols else None
        
        with console.status("[bold blue]正在啟動策略優化..."):
            result = await hft_system.optimize_strategies(symbol_list, target)
        
        if result['success']:
            console.print(f"\n[blue]🔧 策略優化已啟動![/blue]")
            
            table = Table(title="優化工作流")
            table.add_column("交易對", style="cyan")
            table.add_column("優化工作流ID", style="yellow")
            table.add_column("當前表現", style="green")
            table.add_column("狀態")
            
            for symbol, info in result['optimization_results'].items():
                if info['success']:
                    perf = info['current_performance']
                    perf_text = f"Sharpe: {perf['sharpe_ratio']:.2f}"
                    table.add_row(
                        symbol,
                        info['optimization_workflow_id'][:8] + "...",
                        perf_text,
                        "✅ 已啟動"
                    )
                else:
                    table.add_row(symbol, "N/A", "N/A", f"❌ {info['error']}")
            
            console.print(table)
            console.print(f"\n📊 總計優化任務: {result['total_optimizations']}")
        else:
            console.print(f"[red]❌ 優化啟動失敗: {result['error']}[/red]")
    
    asyncio.run(_optimize())


@cli.command()
@click.option('--follow', '-f', is_flag=True, help='持續監控模式')
@click.option('--interval', '-i', default=5, help='刷新間隔（秒）')
@click.pass_context
def status(ctx, follow, interval):
    """查看系統狀態"""
    async def _get_status():
        return await hft_system.get_system_status()
    
    def _display_status(status_data):
        console.clear()
        
        # 系統概覽
        overview = Panel.fit(
            f"🚀 HFT 系統狀態\n"
            f"系統狀態: [bold]{status_data['system_state']}[/bold]\n"
            f"運行時間: {status_data['uptime']:.1f} 小時\n"
            f"活躍交易對: {len(status_data['active_symbols'])}\n"
            f"更新時間: {datetime.now().strftime('%H:%M:%S')}",
            title="系統概覽",
            border_style="green"
        )
        console.print(overview)
        
        # 工作流統計
        workflow_stats = status_data.get('workflow_statistics', {})
        stats_table = Table(title="工作流統計")
        stats_table.add_column("指標", style="cyan")
        stats_table.add_column("數值", style="green")
        
        stats_table.add_row("總工作流", str(workflow_stats.get('total_workflows', 0)))
        stats_table.add_row("活躍會話", str(workflow_stats.get('active_sessions', 0)))
        stats_table.add_row("成功率", f"{workflow_stats.get('success_rate', 0):.1f}%")
        stats_table.add_row("平均執行時間", f"{workflow_stats.get('average_execution_time', 0):.1f}s")
        
        console.print(stats_table)
        
        # 策略狀態
        if status_data.get('strategy_status'):
            strategy_table = Table(title="策略狀態")
            strategy_table.add_column("交易對", style="cyan")
            strategy_table.add_column("工作流ID", style="yellow")
            strategy_table.add_column("啟動時間")
            strategy_table.add_column("進度", style="green")
            strategy_table.add_column("狀態")
            
            for symbol, info in status_data['strategy_status'].items():
                workflow_status = info.get('workflow_status', {})
                if workflow_status.get('found'):
                    progress_info = workflow_status.get('progress', {})
                    progress_pct = progress_info.get('percentage', 0)
                    
                    strategy_table.add_row(
                        symbol,
                        info.get('workflow_id', 'N/A')[:8] + "...",
                        info.get('start_time', 'N/A')[11:19],  # 只顯示時間
                        f"{progress_pct}%",
                        info.get('status', 'unknown')
                    )
            
            console.print(strategy_table)
        
        # 性能指標
        performance = status_data.get('performance_metrics', {})
        if performance:
            perf_table = Table(title="性能指標")
            perf_table.add_column("交易對", style="cyan")
            perf_table.add_column("Sharpe比率", style="green")
            perf_table.add_column("最大回撤", style="red")
            perf_table.add_column("勝率", style="blue")
            perf_table.add_column("總收益", style="yellow")
            
            for symbol, metrics in performance.items():
                perf_table.add_row(
                    symbol,
                    f"{metrics.get('sharpe_ratio', 0):.2f}",
                    f"{metrics.get('max_drawdown', 0):.1%}",
                    f"{metrics.get('win_rate', 0):.1%}",
                    f"{metrics.get('total_return', 0):.1%}"
                )
            
            console.print(perf_table)
    
    if follow:
        # 持續監控模式
        async def _monitor():
            while True:
                try:
                    status_data = await _get_status()
                    _display_status(status_data)
                    await asyncio.sleep(interval)
                except KeyboardInterrupt:
                    console.print("\n[yellow]監控已停止[/yellow]")
                    break
                except Exception as e:
                    console.print(f"[red]監控錯誤: {e}[/red]")
                    await asyncio.sleep(interval)
        
        asyncio.run(_monitor())
    else:
        # 單次查詢
        status_data = asyncio.run(_get_status())
        _display_status(status_data)


@cli.command()
@click.option('--workflow-type', '-t', help='工作流類型過濾')
@click.option('--limit', '-l', default=10, help='顯示數量限制')
@click.pass_context
def workflows(ctx, workflow_type, limit):
    """查看工作流歷史"""
    def _display_workflows():
        history = workflow_manager_v2.get_execution_history(workflow_type, limit)
        
        if not history:
            console.print("[yellow]沒有工作流歷史記錄[/yellow]")
            return
        
        table = Table(title=f"工作流歷史 (最近 {len(history)} 條)")
        table.add_column("時間", style="cyan")
        table.add_column("類型", style="yellow")
        table.add_column("會話ID", style="green")
        table.add_column("交易對", style="blue")
        table.add_column("狀態", style="red")
        
        for record in history:
            table.add_row(
                record.get('start_time', 'N/A')[11:19],  # 只顯示時間
                record.get('workflow_type', 'unknown'),
                record.get('session_id', 'N/A')[:8] + "...",
                record.get('symbol', 'N/A'),
                record.get('status', 'unknown')
            )
        
        console.print(table)
        
        # 統計信息
        stats = workflow_manager_v2.get_workflow_statistics()
        console.print(f"\n📊 工作流統計:")
        console.print(f"   總執行次數: {stats['total_executions']}")
        console.print(f"   成功率: {stats['success_rate']:.1%}")
        console.print(f"   活躍會話: {stats['active_sessions']}")
    
    _display_workflows()


@cli.command()
@click.argument('session_id')
@click.pass_context
def workflow_detail(ctx, session_id):
    """查看特定工作流詳情"""
    def _display_workflow_detail():
        status = workflow_manager_v2.get_session_status(session_id)
        
        if not status['found']:
            console.print(f"[red]❌ 工作流 {session_id} 未找到[/red]")
            return
        
        session = status['session']
        progress = status['progress']
        
        # 會話信息
        info_panel = Panel.fit(
            f"工作流ID: {session['session_id']}\n"
            f"類型: {session['workflow_type']}\n"
            f"交易對: {session['symbol']}\n"
            f"狀態: {session['status']}\n"
            f"進度: {progress['completed_tasks']}/{progress['total_tasks']} ({progress['percentage']}%)\n"
            f"創建時間: {session['created_time']}\n"
            f"錯誤次數: {session['error_count']}",
            title="工作流信息",
            border_style="blue"
        )
        console.print(info_panel)
        
        # 任務詳情
        if session.get('tasks'):
            task_table = Table(title="任務詳情")
            task_table.add_column("任務ID", style="cyan")
            task_table.add_column("Agent類型", style="yellow")
            task_table.add_column("動作", style="green")
            task_table.add_column("狀態", style="red")
            task_table.add_column("執行時間")
            
            for task in session['tasks']:
                exec_time = "N/A"
                if task.get('start_time') and task.get('end_time'):
                    start = datetime.fromisoformat(task['start_time'].replace('Z', ''))
                    end = datetime.fromisoformat(task['end_time'].replace('Z', ''))
                    exec_time = f"{(end - start).total_seconds():.1f}s"
                
                task_table.add_row(
                    task['task_id'][:12] + "...",
                    task['agent_type'],
                    task['action'],
                    task['status'],
                    exec_time
                )
            
            console.print(task_table)
    
    _display_workflow_detail()


@cli.command()
@click.pass_context
def config_show(ctx):
    """顯示當前配置"""
    config = ctx.obj['config']
    
    config_data = {
        "系統模式": config.mode.value,
        "交易對": config.symbols,
        "風險限額": config.risk_limits,
        "模型配置": config.model_config,
        "Rust配置": config.rust_hft_config,
        "監控配置": config.monitoring_config
    }
    
    console.print(Panel.fit(
        json.dumps(config_data, indent=2, ensure_ascii=False),
        title="系統配置",
        border_style="yellow"
    ))


@cli.command()
@click.pass_context
def health(ctx):
    """系統健康檢查"""
    async def _health_check():
        with console.status("[bold blue]正在進行系統健康檢查..."):
            # 使用系統內置的健康檢查
            status_data = await hft_system.get_system_status()
        
        health_status = status_data.get('health_status', {})
        
        if health_status.get('passed'):
            console.print("[green]✅ 系統健康狀況良好![/green]")
        else:
            console.print("[red]❌ 系統健康檢查失敗![/red]")
        
        # 顯示健康詳情
        if 'health_score' in health_status:
            console.print(f"健康評分: {health_status['health_score']}")
        
        if 'reason' in health_status:
            console.print(f"檢查結果: {health_status['reason']}")
    
    asyncio.run(_health_check())


if __name__ == '__main__':
    try:
        cli()
    except KeyboardInterrupt:
        console.print("\n[yellow]操作已取消[/yellow]")
        sys.exit(0)
    except Exception as e:
        console.print(f"\n[red]❌ 程序錯誤: {e}[/red]")
        sys.exit(1)