# UV Python 环境管理指南

## 🚀 项目已成功迁移到 UV 环境管理

### ✅ 已完成的配置

1. **项目初始化**
   - 使用 `uv init` 创建现代化的 Python 项目结构
   - 配置了完整的 `pyproject.toml` 文件

2. **依赖管理**
   - 所有依赖都在 `pyproject.toml` 中管理
   - 分离了开发依赖 (`dev`) 和 CLI 依赖 (`cli`)
   - 包含了完整的 HFT 系统依赖栈

3. **开发环境**
   - 虚拟环境：`.venv/`
   - 测试框架：pytest + asyncio + coverage + xdist
   - 代码质量：black + ruff + mypy + isort

4. **基础测试通过**
   - 创建了 `agno` 模块的基础实现
   - 13 个基础功能测试全部通过
   - 性能测试和并行测试正常

### 🛠️ 常用 UV 命令

#### 环境管理
```bash
# 激活虚拟环境
source .venv/bin/activate

# 或直接使用 uv run
export PATH="$HOME/.local/bin:$PATH"
uv run python --version
```

#### 依赖管理
```bash
# 同步所有依赖
uv sync

# 安装开发依赖
uv sync --extra dev

# 安装特定依赖
uv add pandas

# 添加开发依赖
uv add --dev pytest-mock

# 移除依赖
uv remove package-name
```

#### 测试运行
```bash
# 使用项目的测试运行器
uv run python run_tests.py --unit

# 直接使用 pytest
uv run pytest tests/unit/test_basic_functionality.py -v

# 带覆盖率的测试
uv run pytest --cov=agno --cov-report=html

# 并行测试
uv run pytest -n 4
```

#### 代码质量检查
```bash
# 格式化代码
uv run black .
uv run isort .

# Linting
uv run ruff check .

# 类型检查
uv run mypy .
```

### 📦 项目结构

```
agno_hft/
├── pyproject.toml          # 项目配置和依赖
├── .venv/                  # 虚拟环境 (自动创建)
├── agno/                   # 核心 Agno 模块
│   ├── __init__.py
│   ├── agent.py           # Agent 基础类
│   ├── team.py            # Team 管理
│   ├── workflow.py        # 工作流
│   ├── tools.py           # 工具基础类
│   └── models/            # 模型接口
├── workflows/             # 工作流定义
├── ml/                    # 机器学习模块
├── tests/                 # 测试套件
│   ├── conftest.py       # 测试配置
│   ├── unit/             # 单元测试
│   ├── integration/      # 集成测试
│   └── performance/      # 性能测试
├── run_tests.py          # 测试运行器
└── README.md             # 项目文档
```

### 🎯 主要优势

1. **快速安装**: UV 比 pip 快 10-100 倍
2. **确定性构建**: 自动生成 `uv.lock` 确保一致性
3. **内存高效**: 全局缓存避免重复下载
4. **现代标准**: 遵循 PEP 518/621 标准
5. **简化工作流**: 一个工具管理整个生命周期

### 🔧 当前状态

- ✅ 基础环境配置完成
- ✅ 核心依赖安装成功 (76 个包)
- ✅ 开发依赖配置完成 (19 个包)
- ✅ 基础功能测试通过 (13/13)
- ⚠️ 部分复杂模块需要进一步调整
- 📋 待完成：完整的集成测试和性能基准

### 📝 下一步计划

1. **完善测试覆盖率**
   - 修复剩余的导入依赖问题
   - 完成集成测试和性能测试

2. **优化项目结构**
   - 整理模块间的依赖关系
   - 完善错误处理和日志记录

3. **生产环境准备**
   - 容器化配置
   - CI/CD 集成
   - 部署脚本优化

## 🎉 总结

项目已成功迁移到 UV 环境管理，获得了更快的依赖安装速度、更好的依赖解析和现代化的项目结构。基础测试框架运行良好，为后续开发奠定了坚实基础。