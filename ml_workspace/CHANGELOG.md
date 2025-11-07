# Changelog

All notable changes to ML Workspace will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2025-10-08

### Added

- **Project Constitution**: Established comprehensive governance document (`memory/constitution.md`)
  - 5 core principles: Reproducibility First, Feature Registry as Single Source of Truth, Fail-Fast Data Validation, Config-Driven Workflows, Layered Architecture
  - Quality standards: 70% coverage target for utils/components, 50% minimum for lob_core
  - Performance benchmarks: <30s feature extraction for 1M rows
  - Governance rules for feature additions and breaking changes

- **Configuration Management**:
  - `configs/unsup_train.yaml` - TCN-GRU unsupervised training configuration
  - `configs/backtest.yaml` - Deep learning backtesting parameters
  - `configs/README.md` - Comprehensive configuration usage guide

- **Development Tools**:
  - `.pre-commit-config.yaml` - Pre-commit hooks for Black, Ruff, Bandit, MyPy
  - `pyproject.toml` - Centralized tool configuration for all formatters and linters
  - `pytest.ini` - pytest configuration with asyncio fixture scope

- **CI/CD**:
  - GitHub Actions workflow with 3 jobs: lint, test, feature-verify
  - Multi-version testing (Python 3.11, 3.12)
  - Coverage reporting with Codecov integration
  - Feature registry contract validation

### Fixed

- `__init__.py` - Removed broken imports that caused ModuleNotFoundError
- pytest warnings - Configured asyncio_default_fixture_loop_scope

### Infrastructure

- Established reproducible development environment
- Set code quality baselines: Black (line-length 100), Ruff (E/W/F/I/N/UP/B/C4)
- Configured security scanning with Bandit
- Type checking enabled with MyPy (excluding lob_core research code)

---

## Versioning Strategy

- **Major (X.0.0)**: Breaking changes to feature registry contracts or CLI interfaces
- **Minor (1.X.0)**: New features, strategies, or significant workflow additions
- **Patch (1.0.X)**: Bug fixes, documentation updates, performance optimizations

## Categories

- `Added` - New features or capabilities
- `Changed` - Changes to existing functionality
- `Deprecated` - Features that will be removed in future versions
- `Removed` - Features that have been removed
- `Fixed` - Bug fixes
- `Security` - Security-related changes
- `Infrastructure` - DevOps, CI/CD, tooling improvements
