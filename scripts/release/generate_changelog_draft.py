"""Compose the GitHub Release notes for a USTBL version.

The notes come from `.github/release-notes/v<version>.md` (HTML comments removed) when
the maintainer has prepared one. Otherwise, a fallback draft lists every non-merge commit included in the
release as `<short sha> <subject>`. A Full Changelog link is appended in both cases.

Usage:
  python scripts/release/generate_changelog_draft.py --version 0.4.5 \
      --repository LYOfficial/USTBL [--previous-tag v0.4.4] [--head HEAD] [--output notes.md]
"""

import argparse
import os
import re
import subprocess
import sys

VERSION_PATTERN = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")
TAG_PATTERN = re.compile(r"^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")
REPOSITORY_PATTERN = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
NOTES_DIR = os.path.join(".github", "release-notes")


def git(*args):
    result = subprocess.run(
        ["git", *args],
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    return result.stdout.strip()


def find_previous_tag(head, current_tag):
    """Return the nearest reachable v* tag that is not the tag being released."""
    try:
        tags = git("tag", "--merged", head, "--list", "v*", "--sort=-v:refname").splitlines()
    except subprocess.CalledProcessError:
        return None
    for tag in tags:
        tag = tag.strip()
        if tag and tag != current_tag and TAG_PATTERN.match(tag):
            return tag
    return None


def list_commits(previous_tag, head):
    revision = f"{previous_tag}..{head}" if previous_tag else head
    output = git("log", "--no-merges", "--reverse", "--format=%h %s", revision)
    return [line for line in output.splitlines() if line.strip()]


def compose(version, repository, previous_tag, head):
    tag = f"v{version}"
    notes_path = os.path.join(NOTES_DIR, f"{tag}.md")

    if os.path.isfile(notes_path):
        with open(notes_path, encoding="utf-8") as f:
            # Maintainer comments (e.g. copied from TEMPLATE.md) are not published.
            body = re.sub(r"<!--.*?-->", "", f.read(), flags=re.S).strip()
        source = notes_path
    else:
        commits = list_commits(previous_tag, head)
        lines = ["本次发布包含的提交：", ""]
        lines += [f"- {commit}" for commit in commits] or ["- （无新增提交）"]
        body = "\n".join(lines)
        source = "commits"

    if previous_tag and "Full Changelog" not in body:
        body += (
            "\n\n更新日志：\n\n"
            f"**Full Changelog**: https://github.com/{repository}/compare/{previous_tag}...{tag}"
        )

    return body + "\n", source


def main():
    parser = argparse.ArgumentParser(description="Compose USTBL release notes")
    parser.add_argument("--version", required=True, help="Release version without the v prefix")
    parser.add_argument("--repository", required=True, help="owner/repo used for compare links")
    parser.add_argument("--previous-tag", default="", help="Tag to compare against (auto-detect)")
    parser.add_argument("--head", default="HEAD", help="Commit included as the release head")
    parser.add_argument("--output", default="", help="Write notes to this file instead of stdout")
    args = parser.parse_args()

    if not VERSION_PATTERN.match(args.version):
        sys.exit(f"Invalid version: {args.version}")
    if not REPOSITORY_PATTERN.match(args.repository):
        sys.exit(f"Invalid repository: {args.repository}")
    if args.previous_tag and not TAG_PATTERN.match(args.previous_tag):
        sys.exit(f"Invalid previous tag: {args.previous_tag}")

    previous_tag = args.previous_tag or find_previous_tag(args.head, f"v{args.version}")
    notes, source = compose(args.version, args.repository, previous_tag, args.head)

    if args.output:
        with open(args.output, "w", encoding="utf-8", newline="\n") as f:
            f.write(notes)
    else:
        sys.stdout.reconfigure(encoding="utf-8")
        sys.stdout.write(notes)

    print(f"notes source: {source}; previous tag: {previous_tag or '(none)'}", file=sys.stderr)


if __name__ == "__main__":
    main()
