# Claude Code Hooks Configuration

This document describes the custom hooks configured for the Ploy trading system.

## Configuration Location

**IMPORTANT**: These hooks are configured in **project-local settings** (`.claude/settings.local.json`), not global settings. They only run when working in the Ploy repository.

- ✅ **Project-local**: `.claude/settings.local.json` (Ploy only)
- ❌ **Not global**: `~/.claude/settings.json` (would affect all projects)

This ensures hooks only run for Ploy-specific development and don't interfere with other projects.

## Overview

Three automation hooks have been added to improve development workflow and deployment safety:

1. **PostToolUse** - Auto-test after Rust edits
2. **SessionStart** - Production service health check
3. **Stop** - Deployment checklist report

## Hook Details

### 1. PostToolUse: Auto-Test After Rust Edits

**Script**: `~/.claude/hooks/post-rust-edit-test.sh`

**Trigger**: After `Write` or `Edit` tool modifies a Rust source file (`src/**/*.rs`)

**Behavior**:
- Detects Rust file edits in `src/` directory
- Runs `rtk cargo test --quiet` for token-optimized test execution
- Shows ✅ or ❌ status message based on test results
- Only runs if `Cargo.toml` exists (Rust project detection)

**Configuration**:
```json
{
  "matcher": "Write|Edit",
  "hooks": [{
    "type": "command",
    "command": "~/.claude/hooks/post-rust-edit-test.sh",
    "timeout": 120,
    "statusMessage": "Running tests..."
  }]
}
```

**Benefits**:
- Immediate feedback on code changes
- Prevents committing broken code
- Minimal overhead (only runs for Rust files)

### 2. SessionStart: Production Service Health Check

**Script**: `~/.claude/hooks/session-start-check.sh`

**Trigger**: When a new Claude Code session starts

**Behavior**:
- SSH to tango-1-1 (8.221.143.151)
- Checks status of production services:
  - `ploy-strategy-staggered-arb-dryrun`
  - `ploy-strategy-directional-dryrun`
- Reports active/inactive count
- 5-second connection timeout for fast startup

**Configuration**:
```json
{
  "hooks": [{
    "type": "command",
    "command": "~/.claude/hooks/session-start-check.sh",
    "timeout": 10,
    "statusMessage": "Checking production services..."
  }]
}
```

**Output Examples**:
- ✅ `tango-1-1: All 2 services active`
- ⚠️  `tango-1-1: 1/2 services active (inactive: ploy-strategy-directional-dryrun)`

**Requirements**:
- SSH key at `~/.ssh/id_ed25519`
- Network access to tango-1-1

### 3. Stop: Deployment Checklist Report

**Script**: `~/.claude/hooks/stop-deployment-report.sh`

**Trigger**: When session ends (via `/clear`, `/exit`, or Ctrl+C)

**Behavior**:
Generates a comprehensive deployment report with:

1. **Git Changes Summary**
   - Uncommitted changes count
   - Unpushed commits count
   - Recent commit list

2. **CI/CD Status**
   - Latest GitHub Actions workflow runs
   - Status and conclusion for each workflow
   - Requires `gh` CLI

3. **Production Service Health**
   - tango-1-1 service status check
   - Same SSH check as SessionStart hook

4. **Deployment Recommendation**
   - Suggests next actions if unpushed commits exist
   - Provides ready-to-run commands

**Configuration**:
```json
{
  "hooks": [{
    "type": "command",
    "command": "~/.claude/hooks/stop-deployment-report.sh",
    "timeout": 15,
    "statusMessage": "Generating deployment report..."
  }]
}
```

**Example Output**:
```markdown
## 📊 Session Summary

✅ **Working tree**: Clean
📤 **Unpushed commits**: 2

### Recent commits:
- a1b2c3d feat: add directional strategy optimization
- e4f5g6h fix: correct binance feed routing

## 🔄 CI/CD Status

- **Deploy to tango-1-1**: success (2026-04-01)
- **Test**: success (2026-04-01)

## 🖥️  Production Services (tango-1-1)

✅ Service active
✅ Service active

## 🚀 Deployment Recommendation

**Action needed**: Push commits and trigger deployment

```bash
# Push to main
git push origin main

# Trigger tango-1-1 deployment
gh workflow run deploy-tango-1-1.yml --ref main
```
```

## Hook Management

### View All Hooks
```bash
# Open hooks UI
/hooks
```

### Disable a Hook Temporarily
Edit `.claude/settings.local.json` (in the Ploy project root) and comment out the hook entry.

### Test Hooks Manually
```bash
# Test PostToolUse hook
echo '{"tool_name":"Edit","tool_input":{"file_path":"src/main.rs"}}' | \
  ~/.claude/hooks/post-rust-edit-test.sh

# Test SessionStart hook
~/.claude/hooks/session-start-check.sh

# Test Stop hook
~/.claude/hooks/stop-deployment-report.sh
```

### Update Hook Scripts
Hook scripts are located at:
- `~/.claude/hooks/post-rust-edit-test.sh`
- `~/.claude/hooks/session-start-check.sh`
- `~/.claude/hooks/stop-deployment-report.sh`

Edit these files directly to modify hook behavior.

## Troubleshooting

### Hook Not Running

1. **Check settings.local.json syntax**:
   ```bash
   jq . .claude/settings.local.json
   ```

2. **Verify script permissions**:
   ```bash
   ls -l ~/.claude/hooks/*.sh
   # Should show -rwxr-xr-x (executable)
   ```

3. **Test script manually** (see above)

4. **Check hook logs**:
   - Hooks output to stderr
   - Look for error messages in terminal

### SessionStart Hook Fails

- **SSH key not found**: Ensure `~/.ssh/id_ed25519` exists
- **Connection timeout**: Check network access to tango-1-1
- **Permission denied**: Verify SSH key is authorized on tango-1-1

### PostToolUse Hook Slow

- Hook only runs for `src/**/*.rs` files
- Uses `rtk cargo test --quiet` for minimal output
- 120-second timeout should be sufficient
- Consider adding `--lib` flag to skip integration tests

### Stop Hook Missing CI Status

- Requires `gh` CLI: `brew install gh`
- Authenticate: `gh auth login`
- Verify: `gh run list --limit 1`

## Integration with CLAUDE.md

These hooks complement the deployment rules in CLAUDE.md:

- **PostToolUse** enforces "Verification Before Done" principle
- **SessionStart** provides production visibility
- **Stop** ensures deployment checklist is never forgotten

All hooks respect the "Never build on trading hosts" policy by using CI/CD workflows.
