#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path


APACHE_LICENSE = "SPDX-License-Identifier: Apache-2.0"
APACHE_LONG_MARKER = "Licensed under the Apache License, Version 2.0"
IPAM_LICENSE = "SPDX-License-Identifier: MIT AND Apache-2.0"
IPAM_COPYRIGHT = "SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors"
NVIDIA_COPYRIGHT = (
    "SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved."
)
PROPRIETARY_LICENSE = "SPDX-License-Identifier: " + "LicenseRef-NvidiaProprietary"
DEFAULT_COPYRIGHT = "Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved."
HEADER_WINDOW = 4096

BLOCK_COMMENT_EXTENSIONS = {
    ".c",
    ".cc",
    ".cpp",
    ".cs",
    ".cu",
    ".cuh",
    ".go",
    ".h",
    ".hpp",
    ".java",
    ".js",
    ".jsx",
    ".mod",
    ".proto",
    ".rs",
    ".ts",
    ".tsx",
}
HASH_COMMENT_EXTENSIONS = {
    ".bash",
    ".env",
    ".py",
    ".sh",
    ".toml",
    ".yaml",
    ".yml",
    ".zsh",
}
DASH_COMMENT_EXTENSIONS = {".sql"}
HASH_COMMENT_BASENAMES = {".envrc", ".gitignore", "Makefile", "VERSION"}
EXCLUDED_DIRS = {
    ".git",
    ".cache",
    "build",
    "dist",
    "node_modules",
    "target",
    "third-party",
    "third_party",
    "vendor",
}
EXCLUDED_PREFIXES = (
    "temporal-helm/",
)
EXCLUDED_FILE_SUFFIXES = (
    ".dockerignore",
    ".expected",
    ".example",
    ".min.js",
    ".tmpl",
)

COPYRIGHT_RE = re.compile(r"SPDX-FileCopyrightText:\s*(.+)")
BLOCK_PROPRIETARY_RE = re.compile(
    r"\A/\*.*?SPDX-License-Identifier:\s*" + "LicenseRef-NvidiaProprietary" + r".*?\*/\s*",
    re.DOTALL,
)
HASH_PROPRIETARY_RE = re.compile(
    r"\A(?P<shebang>#![^\n]*\n)?(?P<header>(?:#[^\n]*(?:\n|$)){2,40})",
    re.DOTALL,
)


def source_files(repo: Path, include_untracked: list[str] | None = None) -> list[Path]:
    """Return tracked files plus selected untracked generator outputs.

    This keeps generation independent of Git index state without scanning
    unrelated local files.
    """
    # git ls-files reports index entries, which outlive files the caller has already
    # deleted; generate-sdk clears stale SDK output before regenerating.
    output = subprocess.check_output(["git", "ls-files"], cwd=repo, text=True)
    paths = output.splitlines()
    if include_untracked:
        untracked = subprocess.check_output(
            [
                "git",
                "ls-files",
                "--others",
                "--exclude-standard",
                "--",
                *include_untracked,
            ],
            cwd=repo,
            text=True,
        )
        paths.extend(untracked.splitlines())
    paths = (Path(line) for line in dict.fromkeys(paths))
    return [path for path in paths if (repo / path).is_file()]


def is_dockerfile(path: Path) -> bool:
    return path.name == "Dockerfile" or path.name.startswith("Dockerfile.")


def has_shebang(path: Path) -> bool:
    try:
        return path.read_bytes().startswith(b"#!")
    except OSError:
        return False


def is_generated(text: str) -> bool:
    header = text[:HEADER_WINDOW]
    return "Code generated" in header or "DO NOT EDIT" in header or "@generated" in header


def is_ipam_source(path: Path) -> bool:
    return path.as_posix().startswith("ipam/") and path.suffix in {".go", ".proto"}


def is_candidate(repo: Path, path: Path) -> bool:
    if any(part in EXCLUDED_DIRS for part in path.parts):
        return False
    path_text = path.as_posix()
    if any(path_text.startswith(prefix) for prefix in EXCLUDED_PREFIXES):
        return False
    if path_text.endswith(EXCLUDED_FILE_SUFFIXES):
        return False
    if path_text.startswith("ipam/"):
        return is_ipam_source(path)

    full_path = repo / path
    return (
        path.suffix in BLOCK_COMMENT_EXTENSIONS
        or path.suffix in HASH_COMMENT_EXTENSIONS
        or path.suffix in DASH_COMMENT_EXTENSIONS
        or path.name in HASH_COMMENT_BASENAMES
        or is_dockerfile(path)
        or has_shebang(full_path)
    )


def comment_style(path: Path) -> str:
    if path.name in HASH_COMMENT_BASENAMES or is_dockerfile(path):
        return "hash"
    if path.suffix in BLOCK_COMMENT_EXTENSIONS:
        return "block"
    if path.suffix in DASH_COMMENT_EXTENSIONS:
        return "dash"
    if path.suffix in HASH_COMMENT_EXTENSIONS:
        return "hash"
    return "hash"


def copyright_text(text: str) -> str:
    match = COPYRIGHT_RE.search(text[:HEADER_WINDOW])
    if match:
        return match.group(1).strip()
    return DEFAULT_COPYRIGHT


def block_header(copyright: str) -> str:
    return f"""// SPDX-FileCopyrightText: {copyright}
// SPDX-License-Identifier: Apache-2.0

"""


def hash_header(copyright: str) -> str:
    return f"""# SPDX-FileCopyrightText: {copyright}
# SPDX-License-Identifier: Apache-2.0

"""


def dash_header(copyright: str) -> str:
    return f"""-- SPDX-FileCopyrightText: {copyright}
-- SPDX-License-Identifier: Apache-2.0

"""


def ipam_header() -> str:
    return f"""/*
 * {IPAM_COPYRIGHT}
 * {NVIDIA_COPYRIGHT}
 * {IPAM_LICENSE}
 */

"""


def strip_proprietary_hash_header(text: str) -> tuple[str, str]:
    match = HASH_PROPRIETARY_RE.match(text)
    if not match or PROPRIETARY_LICENSE not in match.group("header"):
        return "", text

    shebang = match.group("shebang") or ""
    return shebang, text[match.end() :]


def _next_comment_block(text: str, pos: int, style: str) -> tuple[str, int] | None:
    """Find the next comment block starting at text[pos].

    A comment block is either:
      - a single /* ... */ block (only when style == "block"), or
      - a maximal contiguous run of line-comment lines (//, #, or --) with no
        blank lines breaking it.

    Returns (block_text, end_pos) where end_pos is the index immediately
    after the block's terminating newline, or None if there is no comment
    block starting exactly at pos.
    """
    n = len(text)
    if pos >= n:
        return None
    line_marker = {"block": "//", "hash": "#", "dash": "--"}[style]

    if style == "block" and text.startswith("/*", pos):
        close = text.find("*/", pos + 2)
        if close == -1:
            return None
        end = close + 2
        while end < n and text[end] in " \t":
            end += 1
        if end < n and text[end] == "\n":
            end += 1
        return text[pos:end], end

    if text.startswith(line_marker, pos):
        end = pos
        while end < n and text.startswith(line_marker, end):
            newline = text.find("\n", end)
            if newline == -1:
                end = n
                break
            end = newline + 1
        return text[pos:end], end

    return None


def strip_existing_apache_header(text: str, style: str) -> str:
    """Remove leading comment blocks that contain APACHE_LONG_MARKER.

    Walks the leading comment region one block at a time. Blocks containing
    the long-form Apache marker are dropped together with the blank lines
    immediately before them. Other comment blocks (e.g. "Code generated by
    protoc-gen-go" markers, doc comments, openapi-generator API descriptions)
    are preserved intact. Iteration stops at the first non-comment, non-blank
    line.
    """
    n = len(text)
    if n == 0:
        return text

    pos = 0
    kept: list[str] = []
    stripped_any = False

    while pos < n:
        blank_start = pos
        while pos < n and text[pos] == "\n":
            pos += 1
        blanks = text[blank_start:pos]

        if pos >= n:
            kept.append(blanks)
            break

        block = _next_comment_block(text, pos, style)
        if block is None:
            kept.append(blanks)
            break

        block_text, end_pos = block
        if APACHE_LONG_MARKER in block_text:
            stripped_any = True
        else:
            kept.append(blanks + block_text)
        pos = end_pos

    if not stripped_any:
        return text
    return "".join(kept) + text[pos:]


def has_long_apache_header(text: str, style: str) -> bool:
    """True if a leading comment block contains the long-form Apache marker."""
    return strip_existing_apache_header(text, style) != text


def fix_text(path: Path, text: str) -> str:
    if is_ipam_source(path):
        return add_ipam_header(text)

    copyright = copyright_text(text)
    style = comment_style(path)

    if style == "block":
        match = BLOCK_PROPRIETARY_RE.match(text)
        if match:
            return block_header(copyright) + text[match.end() :].lstrip("\n")
        text = strip_existing_apache_header(text, "block")
        return block_header(copyright) + text.lstrip("\n")

    if style == "dash":
        text = strip_existing_apache_header(text, "dash")
        return dash_header(copyright) + text.lstrip("\n")

    shebang = ""
    body = text
    if text.startswith("#!"):
        shebang, _, body = text.partition("\n")
        shebang += "\n"
        body = body.lstrip("\n")

    proprietary_shebang, stripped_body = strip_proprietary_hash_header(text)
    if proprietary_shebang:
        return proprietary_shebang + hash_header(copyright) + stripped_body.lstrip("\n")

    body = strip_existing_apache_header(body, "hash")
    return shebang + hash_header(copyright) + body.lstrip("\n")


def add_ipam_header(text: str) -> str:
    if text.startswith("// Code generated"):
        lines = text.splitlines(keepends=True)
        split_at = 0
        for index, line in enumerate(lines):
            if line.startswith("//") or not line.strip():
                split_at = index + 1
                continue
            break
        return "".join(lines[:split_at]) + ipam_header() + "".join(lines[split_at:]).lstrip("\n")

    return ipam_header() + text.lstrip("\n")


def ipam_header_missing(text: str) -> bool:
    header = text[:HEADER_WINDOW]
    return not all(marker in header for marker in (IPAM_COPYRIGHT, NVIDIA_COPYRIGHT, IPAM_LICENSE))


def scan(
    repo: Path,
    *,
    fix: bool,
    migrate_only: bool = False,
    include_untracked: list[str] | None = None,
) -> int:
    missing: list[Path] = []
    proprietary: list[Path] = []
    long_form: list[Path] = []
    fixed: list[Path] = []

    for path in source_files(repo, include_untracked):
        if not is_candidate(repo, path):
            continue

        full_path = repo / path
        text = full_path.read_text(errors="ignore")
        needs_fix = False
        is_long_form = False
        if is_ipam_source(path):
            if ipam_header_missing(text):
                missing.append(path)
                needs_fix = True
            else:
                continue
        else:
            header = text[:HEADER_WINDOW]
            style = comment_style(path)
            if PROPRIETARY_LICENSE in header:
                proprietary.append(path)
                needs_fix = True
            elif APACHE_LICENSE not in header:
                missing.append(path)
                needs_fix = True
            elif has_long_apache_header(text, style):
                long_form.append(path)
                is_long_form = True
                needs_fix = True
            else:
                continue

        if migrate_only and not is_long_form:
            continue

        if fix and needs_fix:
            full_path.write_text(fix_text(path, text))
            fixed.append(path)

    if fixed:
        print(f"Updated source headers in {len(fixed)} files.")
        missing = []
        proprietary = []
        long_form = []

    if missing or proprietary or long_form:
        if missing:
            print(f"Files missing Apache-2.0 source headers: {len(missing)}")
            for path in missing:
                print(f"  {path}")
        if proprietary:
            print(f"Files with proprietary source headers: {len(proprietary)}")
            for path in proprietary:
                print(f"  {path}")
        if long_form:
            print(
                f"Files with long-form Apache-2.0 headers (rerun with --fix to shorten): {len(long_form)}"
            )
            for path in long_form:
                print(f"  {path}")
        return 1 if not fix else 0

    print("All checked source files have expected headers.")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Check NVIDIA source files for Apache-2.0 headers.")
    parser.add_argument("--fix", action="store_true", help="Insert or replace Apache-2.0 source headers.")
    parser.add_argument(
        "--migrate-only",
        action="store_true",
        help="With --fix, only shorten existing long-form Apache headers; do not add headers to files that lack one.",
    )
    parser.add_argument(
        "--include-untracked",
        action="append",
        default=[],
        metavar="PATHSPEC",
        help="Also check untracked, non-ignored files matching this Git pathspec.",
    )
    args = parser.parse_args()

    repo = Path(__file__).resolve().parents[1]
    return scan(
        repo,
        fix=args.fix,
        migrate_only=args.migrate_only,
        include_untracked=args.include_untracked,
    )


if __name__ == "__main__":
    sys.exit(main())
