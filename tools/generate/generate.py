#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Unified code generator for NICo automation artifacts.

Usage:
    python tools/generate/generate.py --backend ansible --spec rest-api/openapi/spec.yaml --output ansible/plugins/modules

Adding a new backend: drop backends/<name>.py with a generate(spec, output_dir, **kwargs) function.
"""
import argparse
import importlib
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import load_spec


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--backend', required=True,
                        help='Backend name (must match backends/<name>.py)')
    parser.add_argument('--spec', required=True)
    parser.add_argument('--output', required=True)
    parser.add_argument('--root', default=None, help='Repo root (passed through to backends that need it)')
    parser.add_argument('--docs-output', default=None, help='Directory for generated Markdown doc pages (ansible backend only)')
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()

    spec = load_spec(args.spec)

    try:
        backend_mod = importlib.import_module('backends.%s' % args.backend)
    except ImportError:
        sys.exit('Unknown backend %r — create tools/generate/backends/%s.py to add it.' % (
            args.backend, args.backend,
        ))

    backend_mod.generate(spec, args.output, root=args.root, docs_output=args.docs_output, dry_run=args.dry_run)


if __name__ == '__main__':
    main()
