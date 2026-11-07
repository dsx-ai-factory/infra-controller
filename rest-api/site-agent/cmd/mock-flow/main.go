// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"flag"

	gsv "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/server"
)

// Test the Flow grpc client
func main() {
	toutPtr := flag.Int("tout", 300, "grpc server timeout")
	flag.Parse()
	gsv.FlowTest(*toutPtr)
}
