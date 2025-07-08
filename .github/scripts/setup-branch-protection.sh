#!/bin/bash

# HFT Trading Platform - Branch Protection Setup Script
# 
# This script configures GitHub branch protection rules for the repository
# according to the specifications in branch-protection-config.md

set -euo pipefail

# Configuration
REPO="${GITHUB_REPOSITORY:-proerror77/monday}"
DRY_RUN="${DRY_RUN:-false}"
GITHUB_TOKEN="${GITHUB_TOKEN:-}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Helper functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

check_prerequisites() {
    log_info "Checking prerequisites..."
    
    # Check if gh CLI is installed
    if ! command -v gh &> /dev/null; then
        log_error "GitHub CLI (gh) is not installed. Please install it first:"
        log_error "  macOS: brew install gh"
        log_error "  Linux: See https://github.com/cli/cli#installation"
        exit 1
    fi
    
    # Check if authenticated
    if ! gh auth status &> /dev/null; then
        log_error "Not authenticated with GitHub. Please run: gh auth login"
        exit 1
    fi
    
    # Check if repository exists and accessible
    if ! gh repo view "$REPO" &> /dev/null; then
        log_error "Cannot access repository $REPO. Please check permissions."
        exit 1
    fi
    
    log_success "Prerequisites check passed"
}

create_main_protection() {
    log_info "Configuring main branch protection..."
    
    local cmd="gh api repos/$REPO/branches/main/protection --method PUT"
    
    # Required status checks
    cmd="$cmd --field required_status_checks='{
        \"strict\": true,
        \"contexts\": [
            \"quick-validation\",
            \"rust-tests\", 
            \"python-tests\",
            \"integration-tests\",
            \"performance-tests\",
            \"security-scan\"
        ]
    }'"
    
    # Pull request reviews
    cmd="$cmd --field required_pull_request_reviews='{
        \"required_approving_review_count\": 2,
        \"dismiss_stale_reviews\": true,
        \"require_code_owner_reviews\": true,
        \"require_last_push_approval\": true
    }'"
    
    # Other settings
    cmd="$cmd --field enforce_admins=true"
    cmd="$cmd --field allow_force_pushes=false"
    cmd="$cmd --field allow_deletions=false"
    cmd="$cmd --field required_linear_history=true"
    
    if [[ "$DRY_RUN" == "true" ]]; then
        log_warning "DRY RUN: Would execute: $cmd"
    else
        if eval "$cmd" &> /dev/null; then
            log_success "Main branch protection configured"
        else
            log_error "Failed to configure main branch protection"
            return 1
        fi
    fi
}

create_develop_protection() {
    log_info "Configuring develop branch protection..."
    
    local cmd="gh api repos/$REPO/branches/develop/protection --method PUT"
    
    # Required status checks
    cmd="$cmd --field required_status_checks='{
        \"strict\": true,
        \"contexts\": [
            \"quick-validation\",
            \"rust-tests\",
            \"python-tests\",
            \"integration-tests\"
        ]
    }'"
    
    # Pull request reviews
    cmd="$cmd --field required_pull_request_reviews='{
        \"required_approving_review_count\": 1,
        \"dismiss_stale_reviews\": true,
        \"require_code_owner_reviews\": false
    }'"
    
    # Other settings
    cmd="$cmd --field enforce_admins=false"
    cmd="$cmd --field allow_force_pushes=false"
    cmd="$cmd --field allow_deletions=false"
    
    if [[ "$DRY_RUN" == "true" ]]; then
        log_warning "DRY RUN: Would execute: $cmd"
    else
        if eval "$cmd" &> /dev/null; then
            log_success "Develop branch protection configured"
        else
            log_error "Failed to configure develop branch protection"
            return 1
        fi
    fi
}

create_release_pattern_protection() {
    log_info "Configuring release/* branch protection..."
    
    # Note: GitHub API doesn't support wildcard patterns directly
    # This would need to be done via GitHub Apps or manual configuration
    log_warning "Release branch pattern protection must be configured manually in GitHub UI"
    log_info "Go to: https://github.com/$REPO/settings/branches"
    log_info "Add rule for pattern: release/*"
    log_info "Use the same settings as main branch but with additional required checks:"
    log_info "  - version-check"
    log_info "  - full-test-suite (unit)"
    log_info "  - full-test-suite (integration)"
    log_info "  - full-test-suite (performance)"
    log_info "  - full-test-suite (security)"
    log_info "  - build-artifacts"
    log_info "  - build-docker-images"
}

verify_protection_rules() {
    log_info "Verifying branch protection rules..."
    
    local branches=("main" "develop")
    local all_good=true
    
    for branch in "${branches[@]}"; do
        log_info "Checking $branch branch..."
        
        if gh api repos/$REPO/branches/$branch/protection &> /dev/null; then
            log_success "$branch branch protection is configured"
            
            # Check specific settings
            local required_checks
            required_checks=$(gh api repos/$REPO/branches/$branch/protection --jq '.required_status_checks.contexts[]' 2>/dev/null | wc -l)
            log_info "  Required status checks: $required_checks"
            
            local required_reviews
            required_reviews=$(gh api repos/$REPO/branches/$branch/protection --jq '.required_pull_request_reviews.required_approving_review_count' 2>/dev/null || echo "0")
            log_info "  Required reviews: $required_reviews"
            
        else
            log_error "$branch branch protection is NOT configured"
            all_good=false
        fi
    done
    
    if [[ "$all_good" == "true" ]]; then
        log_success "All branch protection rules verified"
    else
        log_error "Some branch protection rules are missing"
        return 1
    fi
}

create_teams() {
    log_info "Checking if required teams exist..."
    
    local teams=(
        "hft-core-team"
        "rust-experts" 
        "python-team"
        "ml-team"
        "trading-team"
        "performance-team"
        "data-team"
        "devops-team"
        "security-team"
        "testing-team"
        "tech-writers"
        "product-team"
        "ai-team"
    )
    
    for team in "${teams[@]}"; do
        if gh api orgs/proerror77/teams/$team &> /dev/null; then
            log_success "Team $team exists"
        else
            log_warning "Team $team does not exist"
            if [[ "$DRY_RUN" == "false" ]]; then
                log_info "You may need to create this team manually in GitHub"
            fi
        fi
    done
}

generate_summary_report() {
    log_info "Generating summary report..."
    
    cat << EOF

================================================================================
                    BRANCH PROTECTION SETUP SUMMARY
================================================================================

Repository: $REPO
Timestamp: $(date)
Dry Run: $DRY_RUN

Branch Protection Rules:
  ✓ main branch    - Strict protection with 2 reviews required
  ✓ develop branch - Moderate protection with 1 review required  
  ⚠ release/*     - Manual configuration required
  
Required Teams:
  - hft-core-team (global owners)
  - rust-experts (Rust code review)
  - python-team (Python agent code)
  - ml-team (Machine learning components)
  - trading-team (Trading logic and strategies)
  - performance-team (Low-latency optimization)
  - devops-team (Infrastructure and deployment)
  - security-team (Security reviews)

Next Steps:
1. Manually configure release/* branch pattern protection
2. Ensure all required teams exist in your organization
3. Add team members to appropriate teams
4. Test the protection rules with a sample PR

For more details, see:
  - .github/branch-protection-config.md
  - .github/CODEOWNERS

================================================================================
EOF
}

show_usage() {
    cat << EOF
Usage: $0 [options]

Options:
  --dry-run     Show what would be done without making changes
  --repo REPO   Specify repository (default: proerror77/monday)
  --help        Show this help message

Environment Variables:
  GITHUB_TOKEN  GitHub personal access token (or use 'gh auth login')
  DRY_RUN       Set to 'true' for dry run mode

Examples:
  $0                    # Configure protection rules
  $0 --dry-run         # Preview changes without applying
  $0 --repo user/repo  # Configure different repository

EOF
}

main() {
    # Parse command line arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            --dry-run)
                DRY_RUN="true"
                shift
                ;;
            --repo)
                REPO="$2"
                shift 2
                ;;
            --help)
                show_usage
                exit 0
                ;;
            *)
                log_error "Unknown option: $1"
                show_usage
                exit 1
                ;;
        esac
    done
    
    log_info "Starting branch protection setup for $REPO"
    
    if [[ "$DRY_RUN" == "true" ]]; then
        log_warning "Running in DRY RUN mode - no changes will be made"
    fi
    
    # Run setup steps
    check_prerequisites
    create_teams
    create_main_protection
    create_develop_protection
    create_release_pattern_protection
    verify_protection_rules
    generate_summary_report
    
    log_success "Branch protection setup completed!"
}

# Run main function if script is executed directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi