# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
ARG RUNTIME_IMAGE=nvcr.io/nvidia/distroless/cc:v3.2.1-dev@sha256:3f6fe2b7e87f25d8c665d2caf80763ca691a1a6dfd2ac5a6b6f14c161108f0d4

FROM ${DEBIAN_IMAGE} AS builder

RUN apt-get update && apt-get install -y --no-install-recommends unbound
RUN mkdir -p /deps && \
    ldd /usr/sbin/unbound \
      | awk '$2 == "=>" && $3 ~ /^\// { print $3 }' \
      | sort -u | xargs -r cp -t /deps/

FROM ${RUNTIME_IMAGE}

COPY --from=builder /usr/sbin/unbound /usr/sbin/
COPY --from=builder /deps/ /lib/
COPY --from=builder /usr/share/doc/unbound/copyright /usr/share/doc/unbound/copyright

# The copied set includes Debian's libc beside the runtime's own glibc. This
# build-time run is the guard: it fails the image build if the loader cannot
# bring up the binary with the libraries as copied.
RUN ["/usr/sbin/unbound", "-V"]

ENTRYPOINT ["/usr/sbin/unbound"]
# -dd: stay in the foreground and log to stderr; -p: no pidfile, the
# configuration is mounted read-only and the container has no /run.
CMD ["-dd", "-p", "-c", "/etc/unbound/unbound.conf"]
