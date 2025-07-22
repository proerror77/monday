#!/bin/bash

# Test script for voice notifications

echo "Testing voice notifications..."

# Test 1: Basic voice
echo "Test 1: Basic notification"
say -v "Samantha" "Test 1: Basic voice notification" || echo "Voice not available"
sleep 2

# Test 2: Different voices
echo "Test 2: Testing different voices"
for voice in "Alex" "Samantha" "Victoria"; do
    echo "  Testing voice: $voice"
    say -v "$voice" "Hello from $voice" 2>/dev/null || echo "  Voice $voice not available"
    sleep 1.5
done

# Test 3: With sound effects
echo "Test 3: Sound effects"
sounds=("/System/Library/Sounds/Glass.aiff" "/System/Library/Sounds/Tink.aiff" "/System/Library/Sounds/Pop.aiff")
for sound in "${sounds[@]}"; do
    if [[ -f "$sound" ]]; then
        echo "  Playing: $(basename "$sound")"
        afplay "$sound" 2>/dev/null &
        sleep 1
    fi
done

# Test 4: Simulate task completion
echo "Test 4: Simulating task completion"
echo '{"status":"completed","content":"Test task completed","priority":"high"}' | \
    .claude-code/scripts/voice-notify.sh TodoWrite PostToolUse > /dev/null

echo "Voice notification tests completed!"