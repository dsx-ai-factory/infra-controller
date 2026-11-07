// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"fmt"
	"os"

	"github.com/NVIDIA/infra-controller/rest-api/mcp/internal/server"
	"github.com/NVIDIA/infra-controller/rest-api/openapi"
	"github.com/urfave/cli/v2"
)

func main() {
	app := &cli.App{
		Name:  "nico-mcp",
		Usage: "Serve the NICo REST read surface as MCP tools over streamable-HTTP",
		Description: "Serves the NICo REST read surface as MCP tools at the configured\n" +
			"path and listen address. The server is stateless and never emits\n" +
			"text/event-stream responses; every tool/call returns a single JSON\n" +
			"body. Inbound or default credentials are forwarded only to the\n" +
			"configured NICo REST base URL. A caller-selected base_url can use\n" +
			"only a token supplied in the same tool call.",
		Flags: server.ServeFlags(),
		Action: func(c *cli.Context) error {
			return server.Run(c, openapi.Spec)
		},
	}
	if err := app.Run(os.Args); err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
}
