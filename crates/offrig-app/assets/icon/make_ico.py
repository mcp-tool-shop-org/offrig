"""Rebuild offrig.ico from the six PNGs beside this script.

Run from anywhere: python make_ico.py   (needs Pillow)
Each size is stored as given, never resampled from the 256 px image.
"""
from pathlib import Path

from PIL import Image

here = Path(__file__).resolve().parent
sizes = [16, 32, 48, 64, 128, 256]
frames = [Image.open(here / f"offrig_{s}.png").convert("RGBA") for s in sizes]
frames[-1].save(
    here / "offrig.ico",
    format="ICO",
    sizes=[(s, s) for s in sizes],
    append_images=frames[:-1],
)
