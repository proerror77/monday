#!/bin/bash

# Advanced voice notification script with priority support
# This script provides different voices and sounds based on task priority

# Parse arguments
TOOL_NAME="$1"
EVENT_TYPE="$2"
shift 2
ARGS="$@"

# Configuration
declare -A PRIORITY_VOICES=(
    ["high"]="Alex"
    ["medium"]="Samantha"
    ["low"]="Victoria"
)

declare -A PRIORITY_SOUNDS=(
    ["high"]="/System/Library/Sounds/Glass.aiff"
    ["medium"]="/System/Library/Sounds/Tink.aiff"
    ["low"]="/System/Library/Sounds/Pop.aiff"
)

# Function to extract priority from task
extract_priority() {
    local output="$1"
    local priority=$(echo "$output" | grep -B3 '"status":"completed"' | grep '"priority"' | head -1 | sed -E 's/.*"priority":"([^"]+)".*/\1/')
    if [[ -z "$priority" ]]; then
        echo "medium"
    else
        echo "$priority"
    fi
}

# Function to count completed tasks by priority
count_by_priority() {
    local output="$1"
    local high=$(echo "$output" | grep -B3 '"status":"completed"' | grep -c '"priority":"high"' || echo "0")
    local medium=$(echo "$output" | grep -B3 '"status":"completed"' | grep -c '"priority":"medium"' || echo "0")
    local low=$(echo "$output" | grep -B3 '"status":"completed"' | grep -c '"priority":"low"' || echo "0")
    
    echo "$high:$medium:$low"
}

# Function to extract task details
extract_task_details() {
    local output="$1"
    # Extract all completed task contents
    local tasks=$(echo "$output" | grep -B3 '"status":"completed"' | grep '"content"' | sed -E 's/.*"content":"([^"]+)".*/\1/' | head -3)
    echo "$tasks"
}

# Function to generate detailed message
generate_detailed_message() {
    local tool="$1"
    local priority_counts="$2"
    local task_details="$3"
    
    IFS=':' read -r high medium low <<< "$priority_counts"
    local total=$((high + medium + low))
    
    if [[ "$tool" == "TodoWrite" && "$total" -gt 0 ]]; then
        local message=""
        
        # Build priority summary
        if [[ "$high" -gt 0 ]]; then
            message="${high} high priority"
        fi
        if [[ "$medium" -gt 0 ]]; then
            [[ -n "$message" ]] && message+=", "
            message+="${medium} medium priority"
        fi
        if [[ "$low" -gt 0 ]]; then
            [[ -n "$message" ]] && message+=", "
            message+="${low} low priority"
        fi
        
        if [[ "$total" -eq 1 ]]; then
            # Single task - include details
            local task_name=$(echo "$task_details" | head -1)
            if [[ -n "$task_name" ]]; then
                echo "Completed: $task_name"
            else
                echo "Task completed"
            fi
        else
            # Multiple tasks
            echo "$total tasks completed: $message"
        fi
    else
        # Non-TodoWrite tools
        case "$tool" in
            "Write") echo "File created" ;;
            "Edit"|"MultiEdit") echo "Changes saved" ;;
            "Bash") echo "Command finished" ;;
            *) echo "Done" ;;
        esac
    fi
}

# Function to notify with priority awareness
priority_notify() {
    local message="$1"
    local priority="$2"
    
    # Get voice and sound for priority
    local voice="${PRIORITY_VOICES[$priority]:-Samantha}"
    local sound="${PRIORITY_SOUNDS[$priority]:-/System/Library/Sounds/Tink.aiff}"
    
    # Play sound effect (in background)
    if [[ -f "$sound" ]]; then
        afplay "$sound" 2>/dev/null &
    fi
    
    # Small delay to let sound start
    sleep 0.1
    
    # Speak the message with appropriate voice
    say -v "$voice" -r 180 "$message" 2>/dev/null || echo "[$priority] $message"
}

# Main logic
if [[ "$EVENT_TYPE" == "PostToolUse" ]]; then
    # Read the tool output from stdin
    TOOL_OUTPUT=$(cat)
    
    # Extract priority information
    PRIORITY=$(extract_priority "$TOOL_OUTPUT")
    PRIORITY_COUNTS=$(count_by_priority "$TOOL_OUTPUT")
    TASK_DETAILS=$(extract_task_details "$TOOL_OUTPUT")
    
    # Generate message
    MESSAGE=$(generate_detailed_message "$TOOL_NAME" "$PRIORITY_COUNTS" "$TASK_DETAILS")
    
    # Check if we should notify
    IFS=':' read -r h m l <<< "$PRIORITY_COUNTS"
    TOTAL=$((h + m + l))
    
    if [[ "$TOTAL" -gt 0 ]] || [[ "$TOOL_NAME" != "TodoWrite" ]]; then
        priority_notify "$MESSAGE" "$PRIORITY"
    fi
    
    # Pass through the original output
    echo "$TOOL_OUTPUT"
fi