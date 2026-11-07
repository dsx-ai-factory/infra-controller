# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

FROM alpine

# Libraries needed for go binaries
RUN apk add libc6-compat

COPY build/powershelf-manager /opt/powershelf_manager/powershelf_manager

EXPOSE 50051

CMD /opt/powershelf_manager/powershelf_manager serve
