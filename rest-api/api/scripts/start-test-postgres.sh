#!/bin/bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

#

# Durability is intentionally disabled: this container is ephemeral and the
# database is recreated for every test run. Do NOT copy these flags to any
# non-test environment.
docker run -d --rm --name project-test -p 30432:5432 \
    -e POSTGRES_PASSWORD=postgres -e POSTGRES_USER=postgres -e POSTGRES_DB=project \
    postgres:14.4-alpine \
    -c fsync=off \
    -c synchronous_commit=off \
    -c full_page_writes=off \
    -c wal_level=minimal \
    -c max_wal_senders=0
