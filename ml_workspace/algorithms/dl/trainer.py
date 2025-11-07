#!/usr/bin/env python3
from __future__ import annotations

import math
import os
import time
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Any, Dict, Optional, Tuple

import numpy as np
import torch
from torch import nn
from torch.utils.data import DataLoader, TensorDataset


@dataclass
class DLTrainConfig:
    # Model
    d_model: int = 64
    nhead: int = 4
    num_layers: int = 2
    dropout: float = 0.1
    # Train
    batch_size: int = 256
    epochs: int = 5
    lr: float = 1e-3
    weight_decay: float = 1e-4
    # Reproducibility
    random_seed: int = 42
    # Display
    show_progress: bool = True
    log_interval: int = 50
    return_history: bool = True
    rich_progress: bool = False
    # Export
    export_torchscript: bool = True
    export_onnx: bool = False


class PositionalEncoding(nn.Module):
    def __init__(self, d_model: int, max_len: int = 2048):
        super().__init__()
        pe = torch.zeros(max_len, d_model)
        position = torch.arange(0, max_len, dtype=torch.float).unsqueeze(1)
        div_term = torch.exp(torch.arange(0, d_model, 2).float() * (-math.log(10000.0) / d_model))
        pe[:, 0::2] = torch.sin(position * div_term)
        pe[:, 1::2] = torch.cos(position * div_term)
        pe = pe.unsqueeze(0)
        self.register_buffer("pe", pe)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        # x: [B, T, C]
        t = x.size(1)
        return x + self.pe[:, :t, :]


class SequenceRegressor(nn.Module):
    """Simple Transformer-based sequence regressor.

    Input:  [B, T, F]
    Output: [B] (next-step price change regression)
    """

    def __init__(self, feature_dim: int, d_model: int = 64, nhead: int = 4, num_layers: int = 2, dropout: float = 0.1):
        super().__init__()
        self.input_proj = nn.Linear(feature_dim, d_model)
        self.pos_enc = PositionalEncoding(d_model)

        encoder_layer = nn.TransformerEncoderLayer(d_model=d_model, nhead=nhead, dim_feedforward=d_model * 4, dropout=dropout, batch_first=True)
        self.encoder = nn.TransformerEncoder(encoder_layer, num_layers=num_layers)

        self.head = nn.Sequential(
            nn.LayerNorm(d_model),
            nn.Linear(d_model, d_model),
            nn.ReLU(),
            nn.Dropout(dropout),
            nn.Linear(d_model, 1),
        )

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        # x: [B, T, F]
        h = self.input_proj(x)
        h = self.pos_enc(h)
        h = self.encoder(h)  # [B, T, d]
        # Global average pool across time
        h = h.mean(dim=1)
        out = self.head(h).squeeze(-1)
        return out


def _to_device(*tensors: Optional[torch.Tensor], device: torch.device) -> Tuple[Optional[torch.Tensor], ...]:
    return tuple(t.to(device) if t is not None else None for t in tensors)


def _compute_metrics(pred: np.ndarray, y: np.ndarray) -> Dict[str, float]:
    # IC (Pearson correlation)
    if len(pred) > 1 and np.std(pred) > 0 and np.std(y) > 0:
        ic = float(np.corrcoef(pred, y)[0, 1])
    else:
        ic = 0.0

    # Sign accuracy
    sign_acc = float(np.mean(np.sign(pred) == np.sign(y))) if len(pred) else 0.0

    # IR (Sharpe-like over 1-step pnl = sign(pred)*y)
    pnl = np.sign(pred) * y
    mean = float(np.mean(pnl)) if len(pnl) else 0.0
    std = float(np.std(pnl) + 1e-12)
    ir = mean / std

    # Max drawdown on cumulative pnl
    cum = np.cumsum(pnl)
    if len(cum):
        running_max = np.maximum.accumulate(cum)
        dd = running_max - cum
        max_dd = float(np.max(dd))
    else:
        max_dd = 0.0

    return {"accuracy": sign_acc, "ic": ic, "ir": ir, "max_drawdown": max_dd}


class DLTrainer:
    """Deep learning trainer used by the ML workflow.

    Expects features dict from FeatureEngineer.extract_features_for_training.
    Returns model path and metrics compatible with downstream evaluators.
    """

    def __init__(self, model_dir: str | os.PathLike = "ml_workspace/models", config: Optional[Dict[str, Any]] = None):
        self.model_dir = Path(model_dir)
        self.model_dir.mkdir(parents=True, exist_ok=True)
        self.cfg = DLTrainConfig(**(config or {}))

    def _build_model(self, feature_dim: int) -> nn.Module:
        return SequenceRegressor(
            feature_dim=feature_dim,
            d_model=self.cfg.d_model,
            nhead=self.cfg.nhead,
            num_layers=self.cfg.num_layers,
            dropout=self.cfg.dropout,
        )

    def train(self, features: Dict[str, Any]) -> Dict[str, Any]:
        # Set reproducible seed
        from utils.reproducibility import set_global_seed, log_reproducibility_info
        actual_seed = set_global_seed(self.cfg.random_seed)
        
        # Unpack features
        train_x: torch.Tensor = features["train_features"]  # [N, T, F]
        train_y: torch.Tensor = features["train_labels"]    # [N]
        val_x: torch.Tensor = features["val_features"]
        val_y: torch.Tensor = features["val_labels"]
        seq_len = int(features.get("sequence_length", train_x.shape[1]))
        feat_dim = int(features.get("feature_dim", train_x.shape[2]))

        device = torch.device("cuda" if torch.cuda.is_available() else "cpu")

        model = self._build_model(feat_dim).to(device)
        opt = torch.optim.AdamW(model.parameters(), lr=self.cfg.lr, weight_decay=self.cfg.weight_decay)
        loss_fn = nn.MSELoss()

        train_ds = TensorDataset(train_x, train_y)
        val_ds = TensorDataset(val_x, val_y)
        train_loader = DataLoader(train_ds, batch_size=self.cfg.batch_size, shuffle=True, drop_last=False)
        val_loader = DataLoader(val_ds, batch_size=self.cfg.batch_size, shuffle=False, drop_last=False)

        best_val = float("inf")
        best_state = None

        # Optional progress bar support (tqdm or rich-tqdm)
        tqdm = None  # type: ignore
        use_pbar = False
        if self.cfg.show_progress:
            try:
                if self.cfg.rich_progress:
                    from tqdm.rich import tqdm as tqdm  # type: ignore
                else:
                    from tqdm import tqdm  # type: ignore
                use_pbar = True
            except Exception:
                tqdm = None  # type: ignore
                use_pbar = False

        history: list[dict[str, float]] = []

        epoch_iter = range(self.cfg.epochs)
        if use_pbar and tqdm is not None:
            epoch_iter = tqdm(epoch_iter, desc="Training", unit="epoch")  # type: ignore

        for epoch in epoch_iter:  # type: ignore[arg-type]
            model.train()
            total = 0.0
            seen = 0
            for i, (bx, by) in enumerate(train_loader):
                bx, by = _to_device(bx, by, device=device)
                opt.zero_grad(set_to_none=True)
                pred = model(bx)
                loss = loss_fn(pred, by)
                loss.backward()
                torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
                opt.step()
                bsz = bx.size(0)
                total += float(loss.item()) * bsz
                seen += int(bsz)
                if use_pbar and tqdm is not None and (i + 1) % max(1, self.cfg.log_interval) == 0:
                    avg_loss = total / max(1, seen)
                    gpu_mb = 0
                    if torch.cuda.is_available():
                        try:
                            gpu_mb = int(torch.cuda.memory_allocated() // (1024 * 1024))
                        except Exception:
                            gpu_mb = 0
                    try:
                        epoch_iter.set_postfix({"train_loss": f"{avg_loss:.6f}", "gpu_mb": gpu_mb})  # type: ignore[attr-defined]
                    except Exception:
                        pass

            # Validation
            model.eval()
            vloss = 0.0
            all_pred: list[torch.Tensor] = []
            all_true: list[torch.Tensor] = []
            with torch.no_grad():
                for bx, by in val_loader:
                    bx, by = _to_device(bx, by, device=device)
                    p = model(bx)
                    vloss += float(loss_fn(p, by).item()) * bx.size(0)
                    all_pred.append(p.cpu())
                    all_true.append(by.cpu())

            vloss /= max(1, len(val_ds))
            avg_train = total / max(1, seen)
            if use_pbar and tqdm is not None:
                try:
                    epoch_iter.set_postfix({"train_loss": f"{avg_train:.6f}", "val_loss": f"{vloss:.6f}"})  # type: ignore[attr-defined]
                except Exception:
                    pass

            if self.cfg.return_history:
                history.append({"epoch": float(epoch + 1), "train_loss": float(avg_train), "val_loss": float(vloss)})

            if vloss < best_val:
                best_val = vloss
                best_state = {k: v.detach().cpu().clone() for k, v in model.state_dict().items()}

        # Load best
        if best_state is not None:
            model.load_state_dict(best_state)

        # Final metrics on validation
        model.eval()
        with torch.no_grad():
            vp = model(val_x.to(device)).cpu().numpy()
            vy = val_y.cpu().numpy()
        metrics = _compute_metrics(vp, vy)

        # Save model
        ts = int(time.time())
        model_path = self.model_dir / f"dl_model_{ts}.pt"
        torch.save({
            "state_dict": model.state_dict(),
            "config": asdict(self.cfg),
            "feature_dim": feat_dim,
            "sequence_length": seq_len,
            "random_seed": actual_seed,
            "torch_version": torch.__version__,
        }, model_path)

        # Optional exports
        try:
            if self.cfg.export_torchscript:
                example = torch.zeros(1, seq_len, feat_dim, dtype=torch.float32, device=device)
                scripted = torch.jit.trace(model, example)
                ts_path = self.model_dir / f"dl_model_{ts}_ts.pt"
                scripted.save(str(ts_path))
        except Exception:
            pass

        try:
            if self.cfg.export_onnx:
                onnx_path = self.model_dir / f"dl_model_{ts}.onnx"
                dummy = torch.zeros(1, seq_len, feat_dim, dtype=torch.float32, device=device)
                torch.onnx.export(
                    model,
                    dummy,
                    str(onnx_path),
                    input_names=["input"],
                    output_names=["output"],
                    opset_version=17,
                    dynamic_axes={"input": {0: "batch"}, "output": {0: "batch"}},
                )
        except Exception:
            pass

        return {
            "model_path": str(model_path),
            "metrics": metrics,
            "history": history if self.cfg.return_history else None,
        }


__all__ = ["DLTrainer", "DLTrainConfig"]
