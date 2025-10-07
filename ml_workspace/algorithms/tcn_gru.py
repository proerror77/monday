#!/usr/bin/env python3
"""
TCN-GRU Model Architecture for Unsupervised Learning
Temporal Convolutional Network + Gated Recurrent Unit
"""

import torch
import torch.nn as nn
import torch.nn.functional as F
from copy import deepcopy
from typing import Any, Dict, List, Optional, Tuple


DEFAULT_LOSS_CONFIG: Dict[str, Any] = {
    'reconstruction_weight': 0.4,
    'next_step_weight': 0.15,
    'prediction_weight': 1.0,
    'adaptive_balance': True,
    'slippage': 0.0001,
    'margin': 0.0,
    'direction_temperature': 1.0,
    'adaptive_horizon_scaling': True,
    'cost_weights': {
        'mse_weight': 0.5,
        'direction_weight': 0.25,
        'profit_weight': 0.15,
        'confidence_weight': 0.1,
    },
}


def resolve_loss_config(user_config: Optional[Dict[str, Any]] = None) -> Dict[str, Any]:
    """Merge user-provided loss configuration with sensible defaults."""

    config = deepcopy(DEFAULT_LOSS_CONFIG)
    if not user_config:
        return config

    for key, value in user_config.items():
        if key == 'cost_weights' and isinstance(value, dict):
            config['cost_weights'].update(value)
        elif key in config:
            config[key] = value

    return config


class TCNBlock(nn.Module):
    """Single TCN residual block with dilated causal convolution"""
    
    def __init__(self, in_channels: int, out_channels: int, kernel_size: int, dilation: int, dropout: float = 0.2):
        super().__init__()
        self.conv1 = nn.Conv1d(
            in_channels, out_channels, kernel_size,
            padding=(kernel_size - 1) * dilation, dilation=dilation
        )
        self.conv2 = nn.Conv1d(
            out_channels, out_channels, kernel_size,
            padding=(kernel_size - 1) * dilation, dilation=dilation
        )
        self.dropout = nn.Dropout(dropout)
        self.relu = nn.ReLU()
        
        # Residual connection
        self.residual = nn.Conv1d(in_channels, out_channels, 1) if in_channels != out_channels else None
        
    def forward(self, x: torch.Tensor) -> torch.Tensor:
        """
        Args:
            x: [batch, channels, seq_len]
        Returns:
            [batch, channels, seq_len]
        """
        # First conv block
        out = self.conv1(x)
        # Causal masking: remove future information
        out = out[:, :, :x.size(2)]
        out = self.relu(out)
        out = self.dropout(out)
        
        # Second conv block
        out = self.conv2(out)
        out = out[:, :, :x.size(2)]
        out = self.relu(out)
        out = self.dropout(out)
        
        # Residual connection
        res = x if self.residual is None else self.residual(x)
        return self.relu(out + res)


class TCN(nn.Module):
    """Temporal Convolutional Network"""
    
    def __init__(self, input_size: int, hidden_channels: List[int], kernel_size: int = 3, dropout: float = 0.2):
        super().__init__()
        layers = []
        num_levels = len(hidden_channels)
        
        for i in range(num_levels):
            in_channels = input_size if i == 0 else hidden_channels[i-1]
            out_channels = hidden_channels[i]
            dilation = 2 ** i
            layers.append(TCNBlock(in_channels, out_channels, kernel_size, dilation, dropout))
            
        self.network = nn.Sequential(*layers)
        
    def forward(self, x: torch.Tensor) -> torch.Tensor:
        """
        Args:
            x: [batch, seq_len, features]
        Returns:
            [batch, seq_len, hidden_channels[-1]]
        """
        # TCN expects [batch, channels, seq_len]
        x = x.transpose(1, 2)
        out = self.network(x)
        # Return to [batch, seq_len, channels]
        return out.transpose(1, 2)


class TCNGRU(nn.Module):
    """TCN-GRU model for multi-horizon unsupervised prediction"""
    
    def __init__(
        self,
        input_dim: int,
        tcn_channels: List[int] = [64, 128, 256],
        gru_hidden: int = 128,
        gru_layers: int = 2,
        horizons: List[int] = [1, 5, 10, 30, 60],
        kernel_size: int = 3,
        dropout: float = 0.2
    ):
        super().__init__()
        self.input_dim = input_dim
        self.horizons = horizons
        self.num_horizons = len(horizons)
        
        # TCN encoder
        self.tcn = TCN(input_dim, tcn_channels, kernel_size, dropout)
        
        # GRU decoder
        tcn_output_dim = tcn_channels[-1]
        self.gru = nn.GRU(
            input_size=tcn_output_dim,
            hidden_size=gru_hidden,
            num_layers=gru_layers,
            batch_first=True,
            dropout=dropout if gru_layers > 1 else 0
        )
        
        # Multi-horizon prediction heads
        self.prediction_heads = nn.ModuleList([
            nn.Sequential(
                nn.Linear(gru_hidden, gru_hidden // 2),
                nn.ReLU(),
                nn.Dropout(dropout),
                nn.Linear(gru_hidden // 2, 1)
            ) for _ in horizons
        ])
        
        # Auxiliary tasks for unsupervised learning
        self.reconstruction_head = nn.Linear(gru_hidden, input_dim)
        self.next_step_head = nn.Linear(gru_hidden, input_dim)
        
    def forward(self, x: torch.Tensor, return_features: bool = False):
        """
        Args:
            x: [batch, seq_len, features]
            return_features: Whether to return intermediate features
        Returns:
            For normal mode: (predictions, aux_outputs)
                predictions: [batch, num_horizons] - multi-horizon return predictions
                aux_outputs: dict with reconstruction and next-step predictions
            For TorchScript mode: just predictions tensor
        """
        batch_size, seq_len, _ = x.shape
        
        # TCN encoding
        tcn_out = self.tcn(x)  # [batch, seq_len, tcn_channels[-1]]
        
        # GRU processing
        # Cast to float32 explicitly because GRU on MPS may not support float16
        gru_out, hidden = self.gru(tcn_out.float())  # [batch, seq_len, gru_hidden]
        
        # Use last timestep for predictions
        last_hidden = gru_out[:, -1, :]  # [batch, gru_hidden]
        
        # Multi-horizon predictions
        predictions = []
        for head in self.prediction_heads:
            pred = head(last_hidden)  # [batch, 1]
            predictions.append(pred)
        predictions = torch.cat(predictions, dim=1)  # [batch, num_horizons]
        
        # For TorchScript tracing, only return predictions
        if torch.jit.is_scripting():
            return predictions
        
        # Auxiliary outputs for unsupervised learning
        aux_outputs = {}
        
        # 1. Reconstruction task: reconstruct input sequence
        reconstructed = self.reconstruction_head(gru_out)  # [batch, seq_len, input_dim]
        aux_outputs['reconstruction'] = reconstructed
        
        # 2. Next-step prediction: predict next timestep features
        next_step = self.next_step_head(last_hidden)  # [batch, input_dim]
        aux_outputs['next_step'] = next_step
        
        if return_features:
            aux_outputs['tcn_features'] = tcn_out
            aux_outputs['gru_features'] = gru_out
            aux_outputs['last_hidden'] = last_hidden
        
        return predictions, aux_outputs


class CostAwareLoss(nn.Module):
    """Cost-aware loss that balances magnitude, direction and trading viability."""

    def __init__(
        self,
        taker_fee: float = 0.0006,
        maker_fee: float = 0.0002,
        slippage: float = 0.0001,
        use_maker: bool = False,
        mse_weight: float = 0.5,
        direction_weight: float = 0.25,
        profit_weight: float = 0.15,
        confidence_weight: float = 0.1,
        margin: float = 0.0,
        direction_temperature: float = 1.0,
        adaptive_horizon_scaling: bool = True,
        eps: float = 1e-8
    ):
        super().__init__()
        self.taker_fee = taker_fee
        self.maker_fee = maker_fee
        self.slippage = slippage
        self.use_maker = use_maker
        self.mse_weight = mse_weight
        self.direction_weight = direction_weight
        self.profit_weight = profit_weight
        self.confidence_weight = confidence_weight
        self.margin = margin
        self.direction_temperature = direction_temperature
        self.adaptive_horizon_scaling = adaptive_horizon_scaling
        self.eps = eps
        self.last_components: Dict[str, torch.Tensor] = {}

    def forward(self, predictions: torch.Tensor, targets: torch.Tensor, horizons: List[int]) -> torch.Tensor:
        """Compute cost-aware loss with adaptive scaling across horizons."""

        device = predictions.device
        dtype = predictions.dtype

        fee = self.maker_fee if self.use_maker else self.taker_fee
        round_trip_cost = 2.0 * (fee + self.slippage)

        target_std = torch.clamp(targets.detach().std(dim=0, unbiased=False), min=1e-4)
        if self.adaptive_horizon_scaling:
            scaling = target_std
        else:
            scaling = torch.ones_like(target_std)
        scaling = scaling.to(device=device, dtype=dtype)

        norm_predictions = predictions / scaling
        norm_targets = targets / scaling
        mse_loss = F.mse_loss(norm_predictions, norm_targets)

        net_targets = targets - round_trip_cost
        net_predictions = predictions - round_trip_cost

        std_unsqueezed = target_std.to(device=device, dtype=dtype).unsqueeze(0)
        profit_threshold = (round_trip_cost * std_unsqueezed).to(device=device, dtype=dtype)
        if self.margin > 0:
            profit_threshold = profit_threshold + self.margin

        direction_target = (net_targets > 0).float()
        logits = net_predictions / (std_unsqueezed * self.direction_temperature + self.eps)
        direction_weights = torch.abs(net_targets).detach() / (std_unsqueezed + self.eps)
        direction_loss = F.binary_cross_entropy_with_logits(
            logits,
            direction_target,
            weight=direction_weights + self.eps
        )

        positive_mask = direction_target
        profit_shortfall = F.relu(net_targets - net_predictions - profit_threshold)
        profit_denominator = positive_mask.sum() + self.eps
        profit_loss = (profit_shortfall * positive_mask).sum() / profit_denominator

        negative_mask = 1.0 - direction_target
        confidence_penalty = F.relu(net_predictions + profit_threshold)
        confidence_denominator = negative_mask.sum() + self.eps
        confidence_loss = (confidence_penalty * negative_mask).sum() / confidence_denominator

        total_loss = (
            self.mse_weight * mse_loss
            + self.direction_weight * direction_loss
            + self.profit_weight * profit_loss
            + self.confidence_weight * confidence_loss
        )

        self.last_components = {
            'mse': mse_loss.detach(),
            'direction': direction_loss.detach(),
            'profit_shortfall': profit_loss.detach(),
            'confidence': confidence_loss.detach(),
        }

        return total_loss


class UnsupervisedTCNGRULoss(nn.Module):
    """Combined unsupervised loss for TCN-GRU training"""
    
    def __init__(
        self,
        reconstruction_weight: float = 1.0,
        next_step_weight: float = 0.5,
        prediction_weight: float = 2.0,
        cost_aware_loss: CostAwareLoss = None,
        adaptive_balance: bool = True,
        balance_eps: float = 1e-8
    ):
        super().__init__()
        self.reconstruction_weight = reconstruction_weight
        self.next_step_weight = next_step_weight
        self.prediction_weight = prediction_weight
        self.cost_aware_loss = cost_aware_loss or CostAwareLoss()
        self.adaptive_balance = adaptive_balance
        self.balance_eps = balance_eps
        self.last_scaling: Optional[torch.Tensor] = None
        
    def forward(
        self,
        predictions: torch.Tensor,
        aux_outputs: dict,
        input_sequence: torch.Tensor,
        future_returns: torch.Tensor,
        next_features: torch.Tensor,
        horizons: List[int]
    ) -> dict:
        """
        Calculate combined unsupervised loss
        
        Args:
            predictions: [batch, num_horizons] - multi-horizon predictions
            aux_outputs: Dictionary with reconstruction and next_step
            input_sequence: [batch, seq_len, features] - original input
            future_returns: [batch, num_horizons] - actual future returns
            next_features: [batch, features] - next timestep features
            horizons: List of prediction horizons
            
        Returns:
            Dictionary with individual losses and total loss
        """
        losses = {}
        
        # 1. Reconstruction loss (autoencoder-style)
        reconstruction = aux_outputs['reconstruction']
        reconstruction_loss = F.mse_loss(reconstruction, input_sequence)
        
        # 2. Next-step prediction loss
        next_pred = aux_outputs['next_step']
        next_step_loss = F.mse_loss(next_pred, next_features)
        
        # 3. Multi-horizon prediction loss with costs
        prediction_loss = self.cost_aware_loss(predictions, future_returns, horizons)

        if self.adaptive_balance:
            components = torch.stack([
                reconstruction_loss.detach(),
                next_step_loss.detach(),
                prediction_loss.detach()
            ])
            scaling = components / (components.mean() + self.balance_eps)
            scaling = torch.clamp(scaling, 0.3, 3.0)
        else:
            scaling = torch.ones(3, device=prediction_loss.device, dtype=prediction_loss.dtype)

        if scaling.device != prediction_loss.device:
            scaling = scaling.to(prediction_loss.device)
        if scaling.dtype != prediction_loss.dtype:
            scaling = scaling.to(prediction_loss.dtype)

        losses['reconstruction'] = (self.reconstruction_weight * reconstruction_loss) / (scaling[0] + self.balance_eps)
        losses['next_step'] = (self.next_step_weight * next_step_loss) / (scaling[1] + self.balance_eps)
        losses['prediction'] = (self.prediction_weight * prediction_loss) / (scaling[2] + self.balance_eps)
        self.last_scaling = scaling.detach()
        
        # Total loss
        losses['total'] = sum(losses.values())
        
        return losses
