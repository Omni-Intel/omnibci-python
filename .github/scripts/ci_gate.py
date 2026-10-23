"""Skip duplicate branch validation only when its exact version tag is present."""

import os
import subprocess
import tomllib
from pathlib import Path


def should_run():
    if (
        os.environ.get("GITHUB_EVENT_NAME") != "push"
        or os.environ.get("GITHUB_REF_TYPE") != "branch"
    ):
        return True
    python_version = tomllib.loads(Path("pyproject.toml").read_text())["project"][
        "version"
    ]
    rust_version = tomllib.loads(Path("Cargo.toml").read_text())["package"]["version"]
    if python_version != rust_version:
        raise ValueError("Python and Cargo package versions disagree")
    tags = subprocess.check_output(
        ["git", "tag", "--points-at", os.environ["GITHUB_SHA"]], text=True
    ).splitlines()
    return f"v{python_version}" not in tags


if __name__ == "__main__":
    print(f"should_run={str(should_run()).lower()}")
