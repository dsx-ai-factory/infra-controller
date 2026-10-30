# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Make []string query-filter setters variadic in the generated Go SDK.

Array-typed query parameters (repeatable filters such as ``ipAddress``) get a
``Name(name []string)`` setter from OpenAPI Generator. Making that setter
variadic — ``Name(name ...string)`` — keeps every pre-existing scalar call site
compiling (``IpAddress("10.0.0.1")``) while adding repeatable-filter support
(``IpAddress("10.0.0.1", "10.0.0.2")``); slice callers spread with
``IpAddress(values...)``. The request-builder body is unchanged because a
variadic parameter is already a slice.

The rewrite is restricted to the setters whose scalar signatures this branch
changed — the Instance list filters the spec turned into arrays. Pre-existing
``[]string`` setters (``ApiGetAllNvlinkDomainRequest.Name``,
``ApiGetAllIpxeTemplateRequest.SiteId``) keep their slice signature so
external consumers passing a slice positionally stay source-compatible.
"""

import re
import sys
from pathlib import Path

# (request builder type, setter name) pairs eligible for the variadic rewrite.
_COMPAT_SETTERS = {
    ("ApiGetAllInstanceRequest", "InstanceTypeId"),
    ("ApiGetAllInstanceRequest", "MachineId"),
    ("ApiGetAllInstanceRequest", "IpAddress"),
}

# A generated single-argument setter on a request builder struct.
_SETTER = re.compile(
    r"^func \(r (Api\w+Request)\) (\w+)\((\w+) (\[\]string)\) Api\w+Request \{$",
    re.MULTILINE,
)


def _is_query_parameter(source: str, param_name: str) -> bool:
    """Whether the setter's field is serialized into localVarQueryParams."""
    block = re.search(
        r"\n\tif r\.%s != nil \{\n(.*?)\n\t\}" % re.escape(param_name),
        source,
        re.DOTALL,
    )
    return bool(block) and "localVarQueryParams" in block.group(1)


def main() -> None:
    for filename in sys.argv[1:]:
        path = Path(filename)
        source = path.read_text(encoding="utf-8")

        def to_variadic(match: re.Match[str]) -> str:
            if (match.group(1), match.group(2)) not in _COMPAT_SETTERS:
                return match.group(0)
            param_name = match.group(3)
            if not _is_query_parameter(source, param_name):
                return match.group(0)
            return match.group(0).replace("(%s []string)" % param_name, "(%s ...string)" % param_name)

        formatted = _SETTER.sub(to_variadic, source)
        if formatted != source:
            path.write_text(formatted, encoding="utf-8")


if __name__ == "__main__":
    main()
