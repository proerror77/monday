#!/bin/bash

# 語音完成提示 Hook
# 在 sub agent 完成工作後播放語音提示

# 記錄 hook 執行 (用於調試)
echo "$(date): Voice completion hook started" >> /tmp/voice-hook-debug.log

# 檢查 macOS 是否支援 say 命令
if command -v say >/dev/null 2>&1; then
    # macOS 使用 say 命令
    echo "$(date): Using say command" >> /tmp/voice-hook-debug.log
    say -v Samantha "Sub agent work completed"
elif command -v espeak >/dev/null 2>&1; then
    # Linux 使用 espeak
    echo "$(date): Using espeak" >> /tmp/voice-hook-debug.log
    espeak "Sub agent work completed"
elif command -v spd-say >/dev/null 2>&1; then
    # Linux 使用 speech-dispatcher
    echo "$(date): Using spd-say" >> /tmp/voice-hook-debug.log
    spd-say "Sub agent work completed"
else
    # 如果沒有語音合成，使用系統提示音
    if [[ "$OSTYPE" == "darwin"* ]]; then
        # macOS 系統提示音
        echo "$(date): Using afplay fallback" >> /tmp/voice-hook-debug.log
        afplay /System/Library/Sounds/Glass.aiff
    elif [[ "$OSTYPE" == "linux-gnu"* ]]; then
        # Linux 系統提示音
        echo "$(date): Using Linux audio fallback" >> /tmp/voice-hook-debug.log
        paplay /usr/share/sounds/alsa/Front_Left.wav 2>/dev/null || \
        aplay /usr/share/sounds/alsa/Front_Left.wav 2>/dev/null || \
        echo -e '\a'  # 終端響鈴作為後備方案
    fi
fi

echo "$(date): Voice completion hook completed" >> /tmp/voice-hook-debug.log