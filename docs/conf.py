"""Sphinx configuration; building the docs does not import the native SDK."""

from pathlib import Path
import tomllib

ROOT = Path(__file__).resolve().parents[1]
with (ROOT / "pyproject.toml").open("rb") as stream:
    metadata = tomllib.load(stream)["project"]

project = "OmniBCI Python SDK"
author = "Omni-Intel"
release = metadata["version"]
version = release
language = "zh_CN"
extensions = []
root_doc = "index"
exclude_patterns = ["_build", "Thumbs.db", ".DS_Store"]
html_theme = "alabaster"
html_title = f"{project} {release}"
html_static_path = []
highlight_language = "python"
nitpicky = True
# These external annotation types have no local documentation targets.
nitpick_ignore = [("py:class", "Iterator"), ("py:class", "NDArray")]
