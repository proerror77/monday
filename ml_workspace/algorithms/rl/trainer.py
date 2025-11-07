#!/usr/bin/env python3
from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Dict, Optional


@dataclass
class RLConfig:
    algo: str = "ppo"  # "ppo"|"sac"|"td3"
    total_timesteps: int = 100_000
    seed: int = 42


def _try_import_sb3():
    try:
        import gymnasium as gym  # type: ignore
        from stable_baselines3 import PPO, SAC, TD3  # type: ignore
        from stable_baselines3.common.env_util import make_vec_env  # type: ignore
        return {"gym": gym, "PPO": PPO, "SAC": SAC, "TD3": TD3, "make_vec_env": make_vec_env}
    except Exception:
        return None


class RLTrainer:
    """RL trainer stub with optional SB3/Gym integration.

    To keep dependencies optional, this trainer gracefully degrades if SB3/Gym
    are not available. Integrate with a custom trading env that wraps simulated
    LOB & execution or offline replay.
    """

    def __init__(self, env_id: str = "CartPole-v1", config: Optional[Dict[str, Any]] = None):
        self.env_id = env_id
        self.cfg = RLConfig(**(config or {}))

    def train(self) -> Dict[str, Any]:
        libs = _try_import_sb3()
        if libs is None:
            return {
                "success": False,
                "reason": "stable-baselines3/gymnasium not installed",
                "hint": "pip install gymnasium[all] stable-baselines3",
            }

        gym = libs["gym"]
        PPO = libs["PPO"]
        SAC = libs["SAC"]
        TD3 = libs["TD3"]
        make_vec_env = libs["make_vec_env"]

        # Replace env with real trading env when ready
        env = make_vec_env(self.env_id, n_envs=1, seed=self.cfg.seed)

        if self.cfg.algo.lower() == "ppo":
            model = PPO("MlpPolicy", env, verbose=0, seed=self.cfg.seed)
        elif self.cfg.algo.lower() == "sac":
            model = SAC("MlpPolicy", env, verbose=0, seed=self.cfg.seed)
        else:
            model = TD3("MlpPolicy", env, verbose=0, seed=self.cfg.seed)

        model.learn(total_timesteps=self.cfg.total_timesteps, progress_bar=False)
        path = f"ml_workspace/models/rl_{self.cfg.algo}_model.zip"
        model.save(path)
        return {"success": True, "model_path": path}


__all__ = ["RLTrainer", "RLConfig"]

