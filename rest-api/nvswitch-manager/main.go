// SPDX-FileCopyrightText: Copyright (c) 2021-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/cmd"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/common/log"
)

func main() {
	log.Init()
	cmd.Execute()
}
