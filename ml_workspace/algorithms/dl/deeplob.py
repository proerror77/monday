from __future__ import annotations

import torch
from torch import nn


class DeepLOBNet(nn.Module):
    """Minimal DeepLOB-style CNN encoder for KxL LOB tensors.

    Input shape: [B, C, L, K] where typically C=4 (bid_px_rel, bid_qty_log, ask_px_rel, ask_qty_log)
    Output: class logits (3 classes: -1, 0, +1)
    """

    def __init__(self, k_levels: int = 20, channels: int = 4, num_classes: int = 3):
        super().__init__()
        c = channels
        self.features = nn.Sequential(
            nn.Conv2d(c, 16, kernel_size=(5, 3), padding=(2, 1)),
            nn.ReLU(),
            nn.Conv2d(16, 32, kernel_size=(5, 3), padding=(2, 1)),
            nn.ReLU(),
            nn.Conv2d(32, 32, kernel_size=(3, 3), padding=(1, 1)),
            nn.ReLU(),
            nn.AdaptiveAvgPool2d((16, 1)),  # pool across K to 1, L to 16
        )
        self.head = nn.Sequential(
            nn.Flatten(),
            nn.Linear(32 * 16 * 1, 64),
            nn.ReLU(),
            nn.Linear(64, num_classes),
        )

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        # x: [B, C, L, K]
        h = self.features(x)
        out = self.head(h)
        return out


class MLPLOBNet(nn.Module):
    """MLP over flattened K×L×C tensor.

    Input: [B, C, L, K] -> flatten -> MLP -> 3 classes
    """

    def __init__(self, k_levels: int = 20, L: int = 60, channels: int = 4, hidden: int = 256, num_classes: int = 3):
        super().__init__()
        in_dim = channels * L * k_levels
        self.net = nn.Sequential(
            nn.Flatten(),
            nn.Linear(in_dim, hidden),
            nn.ReLU(),
            nn.Linear(hidden, hidden // 2),
            nn.ReLU(),
            nn.Linear(hidden // 2, num_classes),
        )

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return self.net(x)


class TCNLOBNet(nn.Module):
    """Temporal ConvNet over time axis by flattening K into channels.

    Input: [B, C, L, K] -> reshape to [B, C*K, L] -> 1D dilated convs -> pool -> MLP -> 3 classes
    """

    def __init__(self, k_levels: int = 20, L: int = 60, channels: int = 4, base_channels: int = 32, num_classes: int = 3):
        super().__init__()
        in_ch = channels * k_levels
        self.tcn = nn.Sequential(
            nn.Conv1d(in_ch, base_channels, kernel_size=3, padding=1, dilation=1),
            nn.ReLU(),
            nn.Conv1d(base_channels, base_channels, kernel_size=3, padding=2, dilation=2),
            nn.ReLU(),
            nn.Conv1d(base_channels, base_channels, kernel_size=3, padding=4, dilation=4),
            nn.ReLU(),
            nn.AdaptiveAvgPool1d(16),
        )
        self.head = nn.Sequential(
            nn.Flatten(),
            nn.Linear(base_channels * 16, 64),
            nn.ReLU(),
            nn.Linear(64, num_classes),
        )

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        # x: [B, C, L, K]
        B, C, L, K = x.shape
        x1 = x.permute(0, 1, 3, 2).contiguous().view(B, C * K, L)  # [B, C*K, L]
        h = self.tcn(x1)
        out = self.head(h)
        return out
