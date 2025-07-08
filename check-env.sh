#!/bin/bash

# HFT Trading Platform - Quick Environment Check
# 
# A simple script to quickly check the development environment status

set -euo pipefail

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m'

echo -e "${BLUE}🚀 HFT Trading Platform - Environment Check${NC}"
echo "=================================="

# Current status
echo -e "\n${BLUE}📍 Current Status:${NC}"
echo "Directory: $(pwd)"
echo "Git branch: $(git branch --show-current 2>/dev/null || echo 'Not in git repo')"
echo "Timestamp: $(date)"

# Component check
echo -e "\n${BLUE}🔧 Component Status:${NC}"

# Rust HFT Engine
if [[ -d "rust_hft" ]]; then
    echo -e "Rust HFT Engine: ${GREEN}✓ Available${NC}"
    if [[ -f "rust_hft/Cargo.toml" ]]; then
        echo "  - Cargo.toml: ✓"
    fi
    rust_files=$(find rust_hft/src -name "*.rs" 2>/dev/null | wc -l || echo "0")
    echo "  - Source files: $rust_files .rs files"
else
    echo -e "Rust HFT Engine: ${RED}✗ Missing${NC}"
fi

# Python Agno Framework
if [[ -d "agno_hft" ]]; then
    echo -e "Python Agno Framework: ${GREEN}✓ Available${NC}"
    if [[ -f "agno_hft/hft_agents.py" ]]; then
        echo "  - Agent system: ✓"
    fi
    if [[ -f "agno_hft/requirements.txt" ]]; then
        echo "  - Dependencies: ✓"
    fi
else
    echo -e "Python Agno Framework: ${RED}✗ Missing${NC}"
fi

# Tools check
echo -e "\n${BLUE}🛠️ Development Tools:${NC}"

if command -v rustc &> /dev/null; then
    echo -e "Rust: ${GREEN}✓ $(rustc --version | cut -d' ' -f2)${NC}"
else
    echo -e "Rust: ${RED}✗ Not installed${NC}"
fi

if command -v python3 &> /dev/null; then
    echo -e "Python: ${GREEN}✓ $(python3 --version | cut -d' ' -f2)${NC}"
else
    echo -e "Python: ${RED}✗ Not installed${NC}"
fi

if command -v git &> /dev/null; then
    echo -e "Git: ${GREEN}✓ $(git --version | cut -d' ' -f3)${NC}"
else
    echo -e "Git: ${RED}✗ Not installed${NC}"
fi

# Quick commands
echo -e "\n${BLUE}⚡ Quick Commands:${NC}"
echo "./dev-workflow.sh status    # Detailed status"
echo "./dev-workflow.sh rust      # Enter Rust mode"
echo "./dev-workflow.sh python    # Enter Python mode"
echo "./dev-workflow.sh test      # Run all tests"
echo "./dev-workflow.sh build     # Build all components"

echo -e "\n${GREEN}✅ Environment check completed${NC}"