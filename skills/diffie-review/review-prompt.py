#!/usr/bin/env python3
"""Render open diffie review comments exactly like the app's "Copy all as
prompt" button, by reading the review files directly.

Usage: review-prompt.py [-C REPO] [--source NAME] [--all]

Reads every reviewer's file for one review source, merges reactions across
reviewers (a Done or Won't fix from anyone closes a comment), and prints the
open comments. --all includes closed ones, marked as such.
"""

import argparse
import subprocess
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # tomllib ships with 3.11; older pythons use tomli
    import tomli as tomllib


def slug(name: str) -> str:
    # Mirrors the app's file naming; must match or we look in the wrong folder.
    kept = "".join(c if c.isalnum() or c in "-_." else "-" for c in name)
    return kept.strip("-.") or "unnamed"


def review_root(repo: Path) -> Path:
    # --git-common-dir, not --git-dir: linked worktrees share one review folder.
    out = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "--git-common-dir"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    common = Path(out)
    if not common.is_absolute():
        common = repo / common
    return common / "review"


def load_source(folder: Path):
    comments = []  # (comment, author), in file order per reviewer
    reactions = {}  # comment id -> [kind, ...] across every reviewer
    authors = set()
    for file in sorted(folder.glob("*.toml")):
        data = tomllib.loads(file.read_text())
        author = data.get("review", {}).get("author", file.stem)
        authors.add(author)
        for comment in data.get("comment", []):
            comments.append((comment, author))
        for reaction in data.get("reaction", []):
            reactions.setdefault(reaction["comment"], []).append(reaction["kind"])
    return comments, reactions, authors


def span(comment) -> str:
    last = comment["line"]
    count = comment.get("lines", 1)
    first = last + 1 - min(count, last)
    return str(last) if count <= 1 else f"{first}-{last}"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("-C", dest="repo", default=".", help="repository path")
    parser.add_argument("--source", help="review source (branch name, 'uncommitted', ...)")
    parser.add_argument("--all", action="store_true", help="include resolved comments")
    args = parser.parse_args()

    root = review_root(Path(args.repo))
    folders = [f for f in root.iterdir() if f.is_dir()] if root.is_dir() else []
    if not folders:
        print(f"No reviews under {root}", file=sys.stderr)
        return 1
    if args.source:
        folder = root / slug(args.source)
        if not folder.is_dir():
            names = ", ".join(sorted(f.name for f in folders))
            print(f"No review for '{args.source}'. Sources: {names}", file=sys.stderr)
            return 1
    else:
        # The review the user means is almost always the one touched last.
        folder = max(folders, key=lambda f: max((p.stat().st_mtime for p in f.glob("*.toml")), default=0))
        print(f"Source: {folder.name}  (pick another with --source)", file=sys.stderr)

    comments, reactions, authors = load_source(folder)
    rows = []
    for comment, author in comments:
        kinds = reactions.get(comment["id"], [])
        resolved = any(kind in ("done", "wont_fix") for kind in kinds)
        if resolved and not args.all:
            continue
        rows.append((comment, author, resolved))
    if not rows:
        print("No open review comments.")
        return 0

    out = ["Review comments to address:"]
    for index, (comment, author, resolved) in enumerate(rows, 1):
        header = f"{index}. {comment['path']}:{span(comment)} at {comment['commit'][:7]}"
        if len(authors) > 1:
            header += f" ({author})"
        if resolved:
            header += " [resolved]"
        out.append("")
        out.append(header)
        out.append("```")
        out.append(comment["snippet"].rstrip("\n"))
        out.append("```")
        body = comment["body"].strip()
        if comment.get("selection"):
            body = f"About `{comment['selection']}`: {body}"
        out.append(body)
        if comment.get("suggestion"):
            out.append("Suggested replacement:")
            out.append("```")
            out.append(comment["suggestion"].rstrip("\n"))
            out.append("```")
    print("\n".join(out))
    return 0


if __name__ == "__main__":
    sys.exit(main())
