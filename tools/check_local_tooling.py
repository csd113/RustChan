#!/usr/bin/env python3
"""Reject tracked files covered by the local-tooling block in .gitignore."""

from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    lines = Path(".gitignore").read_text().splitlines()
    start = lines.index(
        "# Local AI tooling: keep instructions, configuration, and sessions on this machine."
    )
    end = lines.index("# End local AI tooling.", start)
    patterns = "\n".join(lines[start + 1 : end]) + "\n"
    with tempfile.NamedTemporaryFile(mode="w+", encoding="utf-8") as excludes:
        excludes.write(patterns)
        excludes.flush()
        result = subprocess.run(
            [
                "git", "ls-files", "--cached", "--ignored", "-z",
                f"--exclude-from={excludes.name}",
            ],
            check=True,
            capture_output=True,
        )
    paths = result.stdout.decode("utf-8", errors="replace").split("\0")
    tracked = [path for path in paths if path]
    if tracked:
        print("Local AI tooling must remain untracked:", file=sys.stderr)
        for path in tracked:
            print(f"  {path!r}", file=sys.stderr)
        return 1
    print("No local AI tooling is tracked.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
