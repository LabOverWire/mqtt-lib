import matplotlib as mpl
import matplotlib.pyplot as plt

MODE_COLORS = {
    "control": "#ff7f0e",
    "per-topic-flow": "#2ca02c",
}

MODE_LABELS = {
    "control": "Control stream",
    "per-topic-flow": "Per-topic flow",
}

MODE_MARKERS = {
    "control": "s",
    "per-topic-flow": "^",
}

MODE_ORDER = ["control", "per-topic-flow"]

LOSS_RATES = [0, 1, 2, 5]
LOSS_LABELS = ["0%", "1%", "2%", "5%"]
RUNS = range(1, 16)

FIGURE_WIDTH = 7
FIGURE_HEIGHT = 4.5
FONT_SIZE = 10


def apply_style():
    mpl.rcParams.update({
        "font.size": FONT_SIZE,
        "font.family": "serif",
        "axes.labelsize": FONT_SIZE + 1,
        "axes.titlesize": FONT_SIZE + 2,
        "xtick.labelsize": FONT_SIZE - 1,
        "ytick.labelsize": FONT_SIZE - 1,
        "legend.fontsize": FONT_SIZE - 1,
        "figure.figsize": (FIGURE_WIDTH, FIGURE_HEIGHT),
        "figure.dpi": 150,
        "savefig.dpi": 300,
        "savefig.bbox": "tight",
        "axes.grid": True,
        "grid.alpha": 0.3,
        "axes.spines.top": False,
        "axes.spines.right": False,
    })


def save_figure(fig, output_dir, name):
    for ext in ["pdf", "png"]:
        path = output_dir / f"{name}.{ext}"
        fig.savefig(path, dpi=300, bbox_inches="tight")
    plt.close(fig)
    print(f"  saved {name}.pdf/.png")
