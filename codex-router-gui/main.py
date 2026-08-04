#!/usr/bin/env python3
"""Backward-compatible launcher for the Codex-Router desktop app."""

import subprocess
import sys
from pathlib import Path


def find_router_root() -> Path:
    start = Path(sys.executable).parent if getattr(sys, "frozen", False) else Path(__file__).resolve().parent.parent
    for candidate in (start, start.parent):
        if (candidate / "scripts" / "Start-Router.ps1").is_file():
            return candidate
    return start


def main() -> None:
    router_root = find_router_root()
    app = router_root / "Codex-Router.exe"
    if not app.is_file():
        raise RuntimeError(f"Codex-Router is missing: {app}")
    subprocess.Popen([str(app)], cwd=str(router_root))


if __name__ == "__main__":
    main()
