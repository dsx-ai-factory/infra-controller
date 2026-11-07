#!/bin/bash
#
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
#

set -euxo pipefail

export DISABLE_TLS_ENFORCEMENT=true
export PGSSLMODE=disable
repo_root=$(git rev-parse --show-toplevel)
export REPO_ROOT=$repo_root

export API_SERVER_HOST="127.0.0.1"
export API_SERVER_PORT="1079"

"$REPO_ROOT/dev/bin/admin-cli.sh" credential add-bmc --kind=site-wide-root --password=pass || echo "Setting BMC site-wide credential failed."
"$REPO_ROOT/dev/bin/admin-cli.sh" credential add-uefi --kind=host --password=pass || echo "Setting uefi password (host) failed."
"$REPO_ROOT/dev/bin/admin-cli.sh" credential add-uefi --kind=dpu --password=pass || echo "Setting uefi password (DPU) failed."

cd "$REPO_ROOT/dev/machine-a-tron/" || exit
cargo run -- "$REPO_ROOT/dev/docker-env/mat.toml"
