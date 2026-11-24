# /// script
# requires-python = ">=3.11"
# dependencies = ["PyYAML==6.0.3"]
# ///
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Stage Fern publication inputs and generate the reference from the current CLI."""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from urllib.parse import urlsplit

import yaml

ROOT = Path(__file__).resolve().parents[2]
MANUAL = Path("docs/manuals/nico-admin-cli")
NAVIGATION = yaml.safe_load(Path(__file__).with_name("navigation.yml").read_text())
OVERVIEW = Path("docs") / NAVIGATION["path"]
OUTPUT = OVERVIEW.parent
INPUTS = ("docs", "fern", "helm-prereqs", "rest-api/openapi", "rest-api/flow/docs", "RELEASE.md")
GITHUB = re.compile(r"https://github\.com/(?:NVIDIA|dsx-ai-factory)/infra-controller/(?:blob|tree)/main/(docs/manuals/nico-admin-cli(?:/[^?#]*)?)$")
LINK = re.compile(r"(\]\(|^ {0,3}\[[^\]\n]+\]:[ \t]+)([^\s()]+)(?=[\s)]|$)")


def read_yaml(path):
    return yaml.safe_load(path.read_text())


def write_yaml(path, value):
    path.write_text(yaml.safe_dump(value, sort_keys=False, allow_unicode=True))


def prepare(site, cli):
    files = subprocess.check_output(["git", "ls-files", "-z", "--", *INPUTS], cwd=ROOT)
    for name in os.fsdecode(files).split("\0"):
        if not name:
            continue
        path = Path(name)
        if path.is_relative_to(MANUAL) or path.is_relative_to("fern/cli-reference") or not (ROOT / path).is_file():
            continue
        (site / path).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / path, site / path)

    with tempfile.TemporaryDirectory(prefix="nico-cli-help-") as scratch:
        raw = Path(scratch) / "reference"
        # Isolate the generator's transient man pages from concurrent previews.
        subprocess.run([*cli, "generate-cli-docs", "--out-dir", str(raw)], cwd=ROOT,
                       env={**os.environ, "TMPDIR": scratch, "REPO_ROOT": str(ROOT)}, check=True)
        shutil.copyfile(ROOT / MANUAL / "README.md", raw / "README.md")
        render(site, raw)


def render(site, raw):
    # The heading preserves the exact command tokens, including hyphenated names.
    domains = read_yaml(ROOT / "crates/admin-cli/cli_domains.yaml")
    folders = {Path(entry["folder"]) for entry in NAVIGATION["contents"]}
    if folders != {OUTPUT.relative_to("docs") / domain for domain in domains}:
        raise ValueError("Fern CLI reference folders must cover every CLI domain")
    by_command = {name: domain for domain, names in domains.items() for name in names}
    commands = {}
    for file in (raw / "commands").glob("*/*.md"):
        heading = file.read_text().splitlines()[0]
        match = re.fullmatch(r"# `nico-admin-cli ([^`]+)`", heading)
        if not match:
            raise ValueError(f"Unrecognized command heading in {file}: {heading}")
        parts = tuple(match[1].split())
        if parts in commands:
            raise ValueError(f"Duplicate command: {parts}")
        commands[parts] = file.relative_to(raw)
    parents = {parts[:-1] for parts in commands}
    pages = {MANUAL / "README.md": (OVERVIEW, NAVIGATION["section"])}
    for domain in domains:
        pages[MANUAL / f"{domain}.md"] = (OUTPUT / domain / "index.md", domain.title())
    for parts, file in commands.items():
        base = OUTPUT.joinpath(by_command[parts[0]], *parts)
        dest = base / "index.md" if parts in parents else base.with_suffix(".md")
        pages[MANUAL / file] = (dest, parts[-1])

    nav_file = site / "docs/index.yml"
    nav = read_yaml(nav_file)
    layout = next(tab["layout"] for tab in nav["navigation"] if tab["tab"] == "documentation")
    reference = next(section for section in layout if section.get("section") == "Reference")
    reference["contents"].insert(0, NAVIGATION)
    dev = next(section for section in reference["contents"] if section.get("section") == "Development")
    saved_slugs = {}
    for entry in dev["contents"]:
        source = Path("docs") / entry.get("path", "")
        if source in pages:
            label = entry["page"]
            slug = entry.get("slug") or label.lower().replace(" ", "-")
            saved_slugs[source] = f"documentation/reference/development/{slug}"
            entry.clear()
            entry.update(link=label, href="/infra-controller/" + saved_slugs[source])

    def github_source(url):
        match = GITHUB.fullmatch(url._replace(query="", fragment="").geturl())
        if not match:
            return None
        source = Path(match[1])
        if not source.suffix:
            source /= "README.md" if source == MANUAL else source.name + ".md"
        if source not in pages:
            raise ValueError(f"No generated reference for {url.geturl()}")
        return source

    def destination(old, new, href):
        url = urlsplit(href)
        src = github_source(url)
        if src is None:
            if url.scheme or url.netloc or not url.path or url.path.startswith("/"):
                return href
            src = Path(os.path.normpath(old.parent / url.path))
        if src not in pages and src.is_relative_to(MANUAL):
            raise ValueError(f"{old}: no generated reference for {href}")
        if src not in pages and old == new:
            return href
        target = pages[src][0] if src in pages else src
        relative = os.path.relpath(target, new.parent)
        return relative + ("?" + url.query if url.query else "") + ("#" + url.fragment if url.fragment else "")

    def markdown(text, old, new):
        lines = []
        fence = None
        for line in text.splitlines(keepends=True):
            marker = re.match(r" {0,3}(`{3,}|~{3,})(.*)", line)
            if fence is None and marker:
                fence = marker[1]
            elif fence and marker and marker[1].startswith(fence) and not marker[2].strip():
                fence = None
            elif fence is None:
                line = LINK.sub(lambda m: m[1] + destination(old, new, m[2]), line)
            lines.append(line)
        return "".join(lines)

    # Links in the staged product navigation use public routes, not source files.
    def update_links(nodes):
        for entry in nodes:
            if "href" in entry:
                url = urlsplit(entry["href"])
                source = github_source(url)
                if source is not None:
                    target = pages[source][0].relative_to(OUTPUT).with_suffix("")
                    if target.name == "index":
                        target = target.parent
                    route = Path("documentation/reference") / NAVIGATION["slug"] / target
                    slug = saved_slugs.get(source, route.as_posix())
                    entry["href"] = url._replace(scheme="", netloc="", path="/infra-controller/" + slug).geturl()
            update_links(entry.get("contents", []))

    update_links(layout)
    write_yaml(nav_file, nav)
    for source, (target, label) in pages.items():
        heading, _, body = (raw / source.relative_to(MANUAL)).read_text().partition("\n")
        meta = {"title": label}
        if source.is_relative_to(MANUAL / "commands"):
            # Fern renders the navigation title as the H1. Keep the complete
            # invocation on the page and in its browser title.
            command = heading.removeprefix("# `").removesuffix("`")
            meta.update(subtitle=command, headline=command)
        if source in saved_slugs:
            meta["slug"] = saved_slugs[source]
        # Quote titles such as "on" and "off" consistently across YAML parsers.
        frontmatter = "".join(f"{k}: {json.dumps(v)}\n" for k, v in meta.items())
        path = site / target
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"---\n{frontmatter}---\n\n" + markdown(body.lstrip("\n"), source, target))

    for directory in ("docs", "fern", "rest-api/flow/docs", "rest-api/openapi", "helm-prereqs"):
        for path in (site / directory).rglob("*"):
            if path.suffix not in (".md", ".mdx") or path.is_relative_to(site / OUTPUT):
                continue
            source = path.relative_to(site)
            original = path.read_text()
            rewritten = markdown(original, source, source)
            if rewritten != original:
                path.write_text(rewritten)
    print(f"Prepared {len(commands)} commands in {len(domains)} domains.", file=sys.stderr)


def check_coverage(cli, site):
    """Compare published command pages with the visible commands in the built CLI."""
    pending = [()]
    expected = set()
    while pending:
        command = pending.pop()
        help_text = subprocess.check_output([cli, *command, "--help"], text=True)
        if command:
            expected.add(" ".join(("nico-admin-cli", *command)))
        section = re.search(r"^Commands:\n((?:[ \t]+[^\n]*\n)+)", help_text, re.MULTILINE)
        if section:
            for name in re.findall(r"^  (\S+)(?=\s|$)", section[1], re.MULTILINE):
                if name != "help":
                    pending.append((*command, name))

    generated = set()
    for path in (site / OUTPUT).rglob("*.md"):
        metadata = yaml.safe_load(path.read_text().split("---", 2)[1])
        if command := metadata.get("headline"):
            if command in generated:
                raise ValueError(f"Duplicate generated command page for {command}")
            generated.add(command)
    if not expected or expected != generated:
        raise ValueError(f"CLI page coverage mismatch: missing {sorted(expected - generated)}, "
                         f"unexpected {sorted(generated - expected)}")
    print(f"Verified a reference page for each of {len(expected)} visible commands.", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, help="Use an existing nico-admin-cli build.")
    parser.add_argument("--check-coverage", action="store_true",
                        help="Check staged pages against --help from the supplied CLI binary.")
    args = parser.parse_args()
    if args.check_coverage and not args.cli:
        parser.error("--check-coverage requires --cli")
    cli = [str(args.cli.resolve())] if args.cli else [
        "cargo", "run", "--locked", "--quiet", "-p", "nico-admin-cli", "--"
    ]
    site = Path(tempfile.mkdtemp(prefix="nico-fern-cli-"))
    try:
        prepare(site, cli)
        if args.check_coverage:
            check_coverage(cli[0], site)
    except BaseException:
        shutil.rmtree(site)
        raise
    print(site)


if __name__ == "__main__":
    main()
