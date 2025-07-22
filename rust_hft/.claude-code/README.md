# Claude Code Voice Notifications

This directory contains voice notification hooks for Claude Code that announce when tasks are completed.

## Features

- 🔊 Voice announcements when tasks are marked as completed
- 🎯 Different voices for different priority levels (high/medium/low)
- 🔔 Optional sound effects
- 📝 Detailed task information in announcements
- 🛠️ Support for multiple Claude Code tools (TodoWrite, Write, Edit, etc.)

## Setup

The voice notifications are already configured in the `settings.json` file. The hooks will automatically trigger when you use Claude Code tools.

## Configuration

### Basic Configuration (settings.json)

The current configuration uses the basic voice notification script for all tools:

```json
{
  "hooks": {
    "PostToolUse": {
      "TodoWrite": ".claude-code/scripts/voice-notify.sh TodoWrite PostToolUse",
      "Write": ".claude-code/scripts/voice-notify.sh Write PostToolUse",
      "Edit": ".claude-code/scripts/voice-notify.sh Edit PostToolUse",
      "MultiEdit": ".claude-code/scripts/voice-notify.sh MultiEdit PostToolUse"
    }
  }
}
```

### Advanced Configuration

To use the advanced script with priority-based voices and sounds, update your `settings.json`:

```json
{
  "hooks": {
    "PostToolUse": {
      "TodoWrite": ".claude-code/scripts/advanced-voice-notify.sh TodoWrite PostToolUse"
    }
  }
}
```

## Available Scripts

1. **voice-notify.sh** - Basic voice notifications
   - Simple task completion announcements
   - Uses Samantha voice by default

2. **advanced-voice-notify.sh** - Priority-aware notifications
   - High priority: Alex voice + Glass sound
   - Medium priority: Samantha voice + Tink sound
   - Low priority: Victoria voice + Pop sound

3. **test-voice.sh** - Test your voice setup
   ```bash
   .claude-code/scripts/test-voice.sh
   ```

## Customization

### Change Voices

Edit the script to use different voices. To see available voices:

```bash
say -v '?'
```

### Disable Sound Effects

In `advanced-voice-notify.sh`, comment out the `afplay` line:

```bash
# afplay "$sound" 2>/dev/null &
```

### Custom Messages

Modify the `generate_message()` or `generate_detailed_message()` functions in the scripts to customize announcements.

## Troubleshooting

1. **No voice output**: Ensure your Mac's volume is turned up and not muted
2. **Permission denied**: Make sure scripts are executable: `chmod +x .claude-code/scripts/*.sh`
3. **Voice not found**: Some voices may need to be downloaded in System Preferences > Accessibility > Spoken Content

## Examples

When you complete a task in Claude Code:

```
# High priority task completed
🔊 "Completed: Implement voice notification system"

# Multiple tasks completed
🔊 "3 tasks completed: 2 high priority, 1 medium priority"

# File operations
🔊 "File created: voice-notify.sh"
🔊 "File edited: settings.json"
```

Enjoy your voice-enabled Claude Code experience! 🎉