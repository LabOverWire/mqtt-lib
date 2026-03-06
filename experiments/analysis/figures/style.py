"""Shared matplotlib/seaborn style configuration for publication figures."""

import matplotlib as mpl
import matplotlib.pyplot as plt

TRANSPORT_COLORS = {
    "tcp": "#1f77b4",
    "quic-control": "#ff7f0e",
    "quic-pertopic": "#2ca02c",
    "quic-perpub": "#d62728",
}

TRANSPORT_LABELS = {
    "tcp": "TCP",
    "quic-control": "QUIC control",
    "quic-pertopic": "QUIC per-topic",
    "quic-perpub": "QUIC per-publish",
}

TRANSPORT_MARKERS = {
    "tcp": "o",
    "quic-control": "s",
    "quic-pertopic": "^",
    "quic-perpub": "D",
}

TRANSPORT_ORDER = ["tcp", "quic-control", "quic-pertopic", "quic-perpub"]

CONN_TRANSPORT_ORDER = ["tcp", "tls", "quic"]
CONN_TRANSPORT_COLORS = {"tcp": "#1f77b4", "tls": "#9467bd", "quic": "#2ca02c"}
CONN_TRANSPORT_LABELS = {"tcp": "TCP", "tls": "TLS 1.3", "quic": "QUIC"}
CONN_TRANSPORT_MARKERS = {"tcp": "o", "tls": "P", "quic": "^"}

THROUGHPUT_ORDER = ["tcp", "quic-control-only", "quic-per-topic", "quic-per-publish"]
THROUGHPUT_COLORS = {
    "tcp": "#1f77b4",
    "quic-control-only": "#ff7f0e",
    "quic-per-topic": "#2ca02c",
    "quic-per-publish": "#d62728",
}
THROUGHPUT_LABELS = {
    "tcp": "TCP",
    "quic-control-only": "QUIC control",
    "quic-per-topic": "QUIC per-topic",
    "quic-per-publish": "QUIC per-publish",
}
THROUGHPUT_MARKERS = {
    "tcp": "o",
    "quic-control-only": "s",
    "quic-per-topic": "^",
    "quic-per-publish": "D",
}

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
