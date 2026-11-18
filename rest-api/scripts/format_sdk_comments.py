# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Wrap enum documentation flattened by OpenAPI Generator's Go templates."""

import re
import sys
import textwrap
from pathlib import Path


def wrap_comment(match: re.Match[str]) -> str:
    return textwrap.fill(
        match[1], width=100, initial_indent="// ", subsequent_indent="// ",
        break_long_words=False, break_on_hyphens=False,
    )


for filename in sys.argv[1:]:
    path = Path(filename)
    source = path.read_text(encoding="utf-8")
    formatted = re.sub(
        r"^// ((\w+) [^\n]+)(?=\ntype \2 \w+\n)",
        wrap_comment, source, flags=re.MULTILINE,
    )
    if formatted != source:
        path.write_text(formatted, encoding="utf-8")
