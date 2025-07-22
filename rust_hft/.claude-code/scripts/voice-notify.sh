#!/bin/bash

# Voice notification script for Claude Code task completions
# This script is called by Claude Code hooks when tasks are completed

# Parse arguments
TOOL_NAME="$1"
EVENT_TYPE="$2"
shift 2
ARGS="$@"

# Function to count completed tasks in TodoWrite output
count_completed_tasks() {
    local output="$1"
    # Look for status changes to completed in the output
    local completed_count=$(echo "$output" | grep -c '"status":"completed"' || echo "0")
    echo "$completed_count"
}

# Function to extract task information from TodoWrite output
extract_task_info() {
    local output="$1"
    # Try to extract task content that was marked as completed
    local task_content=$(echo "$output" | grep -B2 '"status":"completed"' | grep '"content"' | head -1 | sed -E 's/.*"content":"([^"]+)".*/\1/')
    if [[ -n "$task_content" ]]; then
        echo "$task_content"
    else
        echo ""
    fi
}

# Function to generate voice message
generate_message() {
    local task_info="$1"
    local tool="$2"
    local completed_count="$3"
    
    if [[ "$tool" == "TodoWrite" ]]; then
        if [[ "$completed_count" -gt 0 ]]; then
            if [[ "$completed_count" -eq 1 && -n "$task_info" ]]; then
                # Single task with details
                echo "Task completed: $task_info"
            elif [[ "$completed_count" -gt 1 ]]; then
                # Multiple tasks
                echo "$completed_count tasks completed"
            else
                # Single task without details
                echo "Task completed successfully"
            fi
        else
            # Check if it's just a status update
            if [[ "$TOOL_OUTPUT" =~ "in_progress" ]]; then
                echo "Task started"
            else
                echo "Todo list updated"
            fi
        fi
    else
        case "$tool" in
            "Write")
                # Extract filename if possible
                local filename=$(echo "$TOOL_OUTPUT" | grep -oE '/[^/]+$' | head -1)
                if [[ -n "$filename" ]]; then
                    echo "File created: $(basename "$filename")"
                else
                    echo "File written successfully"
                fi
                ;;
            "Edit"|"MultiEdit")
                local filename=$(echo "$TOOL_OUTPUT" | grep -oE '/[^/]+$' | head -1)
                if [[ -n "$filename" ]]; then
                    echo "File edited: $(basename "$filename")"
                else
                    echo "File edited successfully"
                fi
                ;;
            "Bash")
                echo "Command executed"
                ;;
            "Grep"|"Glob")
                echo "Search completed"
                ;;
            *)
                echo "Operation completed"
                ;;
        esac
    fi
}

# Function to play sound and voice notification
notify() {
    local message="$1"
    
    # Play system sound (optional)
    # afplay /System/Library/Sounds/Glass.aiff 2>/dev/null &
    
    # Use different voices for different priorities
    # Available voices: Alex, Samantha, Victoria, etc.
    # Use 'say -v ?' to list all available voices
    
    # Speak the message
    say -v "Samantha" "$message" 2>/dev/null || echo "Voice notification: $message"
}

# Main logic
if [[ "$EVENT_TYPE" == "PostToolUse" ]]; then
    # Read the tool output from stdin
    TOOL_OUTPUT=$(cat)
    
    # Count completed tasks (for TodoWrite)
    COMPLETED_COUNT=$(count_completed_tasks "$TOOL_OUTPUT")
    
    # Extract task information
    TASK_INFO=$(extract_task_info "$TOOL_OUTPUT")
    
    # Generate appropriate message
    MESSAGE=$(generate_message "$TASK_INFO" "$TOOL_NAME" "$COMPLETED_COUNT")
    
    # Send notification only if there's something meaningful to say
    if [[ "$MESSAGE" != "Todo list updated" ]] || [[ "$COMPLETED_COUNT" -gt 0 ]]; then
        notify "$MESSAGE"
    fi
    
    # Pass through the original output
    echo "$TOOL_OUTPUT"
fi