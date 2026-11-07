# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# This is the old Dockerfile that can be used for quick local builds.  See ../docker/production/Dockerfile.nico-flow for the real one.

FROM alpine

# Libraries needed for go binaries
RUN apk add --no-cache libc6-compat

COPY build/flow /opt/flow/flow

EXPOSE 50051 9360

CMD /opt/flow/flow serve
