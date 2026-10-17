#!/usr/bin/env python3
"""Render the runtime-workspace build comparison chart beside this script."""

from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib import font_manager

HERE = Path(__file__).resolve().parent
OUTPUT = HERE / "2026-10-07-rust-build-speed.png"

# Median delta percent versus the jobs=8 baseline. sccache incremental was not
# split by cache temperature, so miss and warm share that one median.
DELTAS = {
    ("jobs-ncpu", "cold-build"): 18.0,
    ("jobs-ncpu", "incremental-build"): 14.2,
    ("jobs-ncpu", "cold-check"): 14.9,
    ("mold", "cold-build"): 0.4,
    ("mold", "incremental-build"): 2.8,
    ("mold", "cold-check"): 1.0,
    ("lld", "cold-build"): 1.2,
    ("lld", "incremental-build"): 4.4,
    ("lld", "cold-check"): 2.7,
    ("line-tables", "cold-build"): 1.0,
    ("line-tables", "incremental-build"): -0.5,
    ("line-tables", "cold-check"): 20.0,
    ("dep-opt1", "cold-build"): 250.4,
    ("dep-opt1", "incremental-build"): -1.4,
    ("dep-opt1", "cold-check"): 117.8,
    ("sccache-miss", "cold-build"): 13.7,
    ("sccache-miss", "incremental-build"): -2.1,
    ("sccache-miss", "cold-check"): 33.7,
    ("sccache-warm", "cold-build"): -59.5,
    ("sccache-warm", "incremental-build"): -2.1,
    ("sccache-warm", "cold-check"): -41.5,
}

# The report table also lists the mixed three-run sccache median. The chart splits miss and warm.
CHART_VARIANTS = [
    ("jobs-ncpu", "jobs=ncpu"),
    ("mold", "mold"),
    ("lld", "lld"),
    ("line-tables", "line-tables"),
    ("dep-opt1", "dep opt=1"),
    ("sccache-miss", "sccache miss"),
    ("sccache-warm", "sccache warm"),
]
SCENARIOS = [
    ("cold-build", "cold build"),
    ("incremental-build", "incremental"),
    ("cold-check", "cargo check"),
]


def main() -> None:
    for font in (
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttf",
    ):
        if Path(font).exists():
            font_manager.fontManager.addfont(font)
            plt.rcParams["font.family"] = font_manager.FontProperties(fname=font).get_name()
            break
    fig, ax = plt.subplots(figsize=(11.5, 6.2))
    width = 0.25
    colors = {"cold-build": "#4C78A8", "incremental-build": "#F58518", "cold-check": "#54A24B"}
    xs = list(range(len(CHART_VARIANTS)))
    for index, (scenario, label) in enumerate(SCENARIOS):
        offsets = [x + (index - 1) * width for x in xs]
        values = [DELTAS[(variant, scenario)] for variant, _ in CHART_VARIANTS]
        bars = ax.bar(offsets, values, width=width, label=label, color=colors[scenario])
        for bar, value in zip(bars, values):
            ax.annotate(
                f"{value:.0f}%",
                xy=(bar.get_x() + bar.get_width() / 2, bar.get_height()),
                xytext=(0, 3 if value >= 0 else -11),
                textcoords="offset points",
                ha="center",
                va="bottom" if value >= 0 else "top",
                fontsize=8,
            )
    ax.axhline(0, color="#222", linewidth=0.8)
    ax.set_xticks(xs)
    ax.set_xticklabels([label for _, label in CHART_VARIANTS])
    ax.set_ylabel("相对 baseline 的中位耗时变化 (%)")
    ax.set_title("runtime workspace 构建对照（负值更快；没有一项写入配置）")
    ax.legend(frameon=False, ncol=3, loc="upper left")
    ax.set_ylim(-80, 290)
    fig.tight_layout()
    fig.savefig(OUTPUT, dpi=140)
    print(OUTPUT)


if __name__ == "__main__":
    main()
