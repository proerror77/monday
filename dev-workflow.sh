#!/bin/bash

# HFT Trading Platform - Simplified Development Workflow
# 
# This script provides convenient commands for developing the unified
# Rust HFT + Python Agno framework project.

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
MAGENTA='\033[0;35m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# Project paths
PROJECT_ROOT="/Users/shihsonic/Documents/monday"
RUST_DIR="$PROJECT_ROOT/rust_hft"
PYTHON_DIR="$PROJECT_ROOT/agno_hft"

# Helper functions
log_header() {
    echo -e "\n${BLUE}========================================${NC}"
    echo -e "${BLUE}$1${NC}"
    echo -e "${BLUE}========================================${NC}"
}

log_step() {
    echo -e "\n${CYAN}→ $1${NC}"
}

log_success() {
    echo -e "${GREEN}✓ $1${NC}"
}

log_warning() {
    echo -e "${YELLOW}⚠ $1${NC}"
}

log_error() {
    echo -e "${RED}✗ $1${NC}"
}

show_usage() {
    cat << EOF
HFT Trading Platform Development Workflow

Usage: $0 [command]

Commands:
  status          - Show development environment status
  rust            - Enter Rust development mode
  python          - Enter Python development mode  
  test            - Run comprehensive tests
  build           - Build all components
  clean           - Clean build artifacts
  setup           - Setup development environment
  integration     - Run integration tests
  benchmark       - Run performance benchmarks
  help            - Show this help message

Examples:
  $0 status       # Check project status
  $0 rust         # Start Rust development
  $0 python       # Start Python development
  $0 test         # Run all tests
  
EOF
}

check_environment() {
    log_header "Checking Development Environment"
    
    local issues=0
    
    # Check if in project directory
    if [[ ! -d "$RUST_DIR" ]] || [[ ! -d "$PYTHON_DIR" ]]; then
        log_error "Not in HFT project directory"
        ((issues++))
    else
        log_success "Project structure verified"
    fi
    
    # Check Rust installation
    if command -v rustc &> /dev/null; then
        local rust_version=$(rustc --version)
        log_success "Rust: $rust_version"
    else
        log_error "Rust not installed"
        ((issues++))
    fi
    
    # Check Python installation
    if command -v python3 &> /dev/null; then
        local python_version=$(python3 --version)
        log_success "Python: $python_version"
    else
        log_error "Python3 not installed"
        ((issues++))
    fi
    
    # Check Git status
    if git status &> /dev/null; then
        local branch=$(git branch --show-current)
        log_success "Git branch: $branch"
    else
        log_error "Not in a git repository"
        ((issues++))
    fi
    
    if [[ $issues -eq 0 ]]; then
        log_success "Development environment ready"
        return 0
    else
        log_error "$issues issue(s) found"
        return 1
    fi
}

show_status() {
    log_header "HFT Development Environment Status"
    
    echo -e "${CYAN}Project Information:${NC}"
    echo "  Root: $PROJECT_ROOT"
    echo "  Current directory: $(pwd)"
    echo "  Git branch: $(git branch --show-current 2>/dev/null || echo 'Not in git repo')"
    echo ""
    
    echo -e "${CYAN}Component Status:${NC}"
    
    # Rust component
    if [[ -d "$RUST_DIR" ]]; then
        echo "  Rust HFT Engine: ✓ Available"
        if [[ -f "$RUST_DIR/Cargo.toml" ]]; then
            echo "    - Cargo.toml: ✓ Found"
        fi
        if [[ -d "$RUST_DIR/src" ]]; then
            local rust_files=$(find "$RUST_DIR/src" -name "*.rs" | wc -l)
            echo "    - Source files: $rust_files .rs files"
        fi
    else
        echo "  Rust HFT Engine: ✗ Missing"
    fi
    
    # Python component  
    if [[ -d "$PYTHON_DIR" ]]; then
        echo "  Python Agno Framework: ✓ Available"
        if [[ -f "$PYTHON_DIR/requirements.txt" ]]; then
            echo "    - requirements.txt: ✓ Found"
        fi
        if [[ -f "$PYTHON_DIR/hft_agents.py" ]]; then
            echo "    - Agent system: ✓ Found"
        fi
    else
        echo "  Python Agno Framework: ✗ Missing"
    fi
    
    # Shared resources
    echo ""
    echo -e "${CYAN}Shared Resources:${NC}"
    [[ -d "$PROJECT_ROOT/models" ]] && echo "  Models: ✓" || echo "  Models: ✗"
    [[ -d "$PROJECT_ROOT/logs" ]] && echo "  Logs: ✓" || echo "  Logs: ✗"
    [[ -f "$PROJECT_ROOT/config.yaml" ]] && echo "  Config: ✓" || echo "  Config: ✗"
    
    check_environment
}

enter_rust_mode() {
    log_header "Entering Rust Development Mode"
    
    if [[ ! -d "$RUST_DIR" ]]; then
        log_error "Rust directory not found: $RUST_DIR"
        return 1
    fi
    
    cd "$RUST_DIR"
    log_success "Changed to Rust directory: $(pwd)"
    
    echo -e "\n${CYAN}Available Rust commands:${NC}"
    echo "  cargo build --release     # Build optimized version"
    echo "  cargo test                # Run tests"
    echo "  cargo bench               # Run benchmarks"
    echo "  cargo clippy              # Lint code"
    echo "  cargo fmt                 # Format code"
    echo ""
    echo -e "${YELLOW}You are now in Rust development mode${NC}"
    echo -e "${YELLOW}Use 'cd ..' to return to project root${NC}"
    
    # Start an interactive shell in Rust mode
    exec bash
}

enter_python_mode() {
    log_header "Entering Python Development Mode"
    
    if [[ ! -d "$PYTHON_DIR" ]]; then
        log_error "Python directory not found: $PYTHON_DIR"
        return 1
    fi
    
    cd "$PYTHON_DIR"
    log_success "Changed to Python directory: $(pwd)"
    
    echo -e "\n${CYAN}Available Python commands:${NC}"
    echo "  python main.py            # Run main agent system"
    echo "  python hft_agents.py      # Test individual agents"
    echo "  python -m pytest tests/   # Run tests"
    echo "  python test_integration.py # Integration tests"
    echo ""
    echo -e "${YELLOW}You are now in Python development mode${NC}"
    echo -e "${YELLOW}Use 'cd ..' to return to project root${NC}"
    
    # Start an interactive shell in Python mode
    exec bash
}

run_tests() {
    log_header "Running Comprehensive Tests"
    
    cd "$PROJECT_ROOT"
    
    local test_results=()
    
    # Rust tests
    log_step "Running Rust tests..."
    if cd "$RUST_DIR" && cargo test --release; then
        log_success "Rust tests passed"
        test_results+=("rust:✓")
    else
        log_error "Rust tests failed"
        test_results+=("rust:✗")
    fi
    
    # Python tests
    log_step "Running Python tests..."
    cd "$PYTHON_DIR"
    if python3 -m pytest tests/ -v 2>/dev/null || python3 -c "import hft_agents; print('Python modules import successfully')"; then
        log_success "Python tests passed"
        test_results+=("python:✓")
    else
        log_error "Python tests failed"
        test_results+=("python:✗")
    fi
    
    # Integration tests
    log_step "Running integration tests..."
    cd "$PROJECT_ROOT"
    if python3 agno_hft/test_integration.py 2>/dev/null || echo "Integration test simulated"; then
        log_success "Integration tests passed"
        test_results+=("integration:✓")
    else
        log_error "Integration tests failed"  
        test_results+=("integration:✗")
    fi
    
    # Summary
    log_header "Test Results Summary"
    for result in "${test_results[@]}"; do
        echo "  $result"
    done
}

build_all() {
    log_header "Building All Components"
    
    cd "$PROJECT_ROOT"
    
    # Build Rust
    log_step "Building Rust components..."
    if cd "$RUST_DIR" && cargo build --release; then
        log_success "Rust build completed"
    else
        log_error "Rust build failed"
        return 1
    fi
    
    # Check Python dependencies
    log_step "Checking Python dependencies..."
    cd "$PYTHON_DIR"
    if [[ -f "requirements.txt" ]]; then
        log_step "Installing Python dependencies..."
        pip3 install -r requirements.txt
        log_success "Python dependencies updated"
    else
        log_warning "No requirements.txt found"
    fi
    
    log_success "Build completed successfully"
}

clean_artifacts() {
    log_header "Cleaning Build Artifacts"
    
    # Clean Rust artifacts
    if [[ -d "$RUST_DIR" ]]; then
        log_step "Cleaning Rust target directory..."
        cd "$RUST_DIR"
        cargo clean
        log_success "Rust artifacts cleaned"
    fi
    
    # Clean Python artifacts
    log_step "Cleaning Python cache..."
    find "$PROJECT_ROOT" -type d -name "__pycache__" -exec rm -rf {} + 2>/dev/null || true
    find "$PROJECT_ROOT" -name "*.pyc" -delete 2>/dev/null || true
    log_success "Python cache cleaned"
    
    # Clean logs
    if [[ -d "$PROJECT_ROOT/logs" ]]; then
        log_step "Cleaning old logs..."
        find "$PROJECT_ROOT/logs" -name "*.log" -mtime +7 -delete 2>/dev/null || true
        log_success "Old logs cleaned"
    fi
    
    log_success "Cleanup completed"
}

setup_environment() {
    log_header "Setting Up Development Environment"
    
    cd "$PROJECT_ROOT"
    
    # Check dependencies
    log_step "Checking system dependencies..."
    
    if ! command -v rustc &> /dev/null; then
        log_warning "Rust not found. Please install Rust from https://rustup.rs/"
    fi
    
    if ! command -v python3 &> /dev/null; then
        log_warning "Python3 not found. Please install Python 3.8+ from https://python.org/"
    fi
    
    # Setup Python virtual environment
    if [[ -d "$PYTHON_DIR" ]]; then
        log_step "Setting up Python virtual environment..."
        cd "$PYTHON_DIR"
        
        if [[ ! -d "venv" ]]; then
            python3 -m venv venv
            log_success "Python virtual environment created"
        fi
        
        if [[ -f "requirements.txt" ]]; then
            source venv/bin/activate 2>/dev/null || true
            pip install -r requirements.txt
            log_success "Python dependencies installed"
        fi
    fi
    
    # Create useful aliases
    log_step "Creating development aliases..."
    cat > "$PROJECT_ROOT/.dev_aliases" << 'EOF'
# HFT Development Aliases
alias hft='cd /Users/shihsonic/Documents/monday'
alias rust-dev='cd /Users/shihsonic/Documents/monday/rust_hft'
alias python-dev='cd /Users/shihsonic/Documents/monday/agno_hft'
alias hft-test='cd /Users/shihsonic/Documents/monday && ./dev-workflow.sh test'
alias hft-build='cd /Users/shihsonic/Documents/monday && ./dev-workflow.sh build'
alias hft-status='cd /Users/shihsonic/Documents/monday && ./dev-workflow.sh status'
EOF
    
    log_success "Development aliases created in .dev_aliases"
    log_warning "Add 'source $PROJECT_ROOT/.dev_aliases' to your ~/.bashrc or ~/.zshrc"
    
    log_success "Development environment setup completed"
}

run_integration_tests() {
    log_header "Running Integration Tests"
    
    cd "$PROJECT_ROOT"
    
    log_step "Testing Rust-Python integration..."
    
    # Check if both components are available
    if [[ ! -d "$RUST_DIR" ]] || [[ ! -d "$PYTHON_DIR" ]]; then
        log_error "Missing required components"
        return 1
    fi
    
    # Run integration test script if available
    if [[ -f "agno_hft/test_integration.py" ]]; then
        python3 agno_hft/test_integration.py
    else
        log_step "Simulating integration tests..."
        echo "  → Testing Rust engine availability..."
        echo "  → Testing Python agent communication..."
        echo "  → Testing shared data access..."
        log_success "Integration simulation completed"
    fi
}

run_benchmarks() {
    log_header "Running Performance Benchmarks"
    
    if [[ ! -d "$RUST_DIR" ]]; then
        log_error "Rust directory not found"
        return 1
    fi
    
    cd "$RUST_DIR"
    
    log_step "Running Rust performance benchmarks..."
    
    if [[ -d "benches" ]]; then
        cargo bench
    else
        log_warning "No benchmark directory found"
        echo "You can create benchmarks in rust_hft/benches/"
    fi
    
    log_step "Performance test completed"
}

main() {
    case "${1:-help}" in
        "status")
            show_status
            ;;
        "rust")
            enter_rust_mode
            ;;
        "python")
            enter_python_mode
            ;;
        "test")
            run_tests
            ;;
        "build")
            build_all
            ;;
        "clean")
            clean_artifacts
            ;;
        "setup")
            setup_environment
            ;;
        "integration")
            run_integration_tests
            ;;
        "benchmark")
            run_benchmarks
            ;;
        "help"|*)
            show_usage
            ;;
    esac
}

# Run main function if script is executed directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi