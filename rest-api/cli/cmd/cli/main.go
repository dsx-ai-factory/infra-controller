// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"fmt"
	"os"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/NVIDIA/infra-controller/rest-api/cli/tui"
	"github.com/NVIDIA/infra-controller/rest-api/openapi"
	"github.com/urfave/cli/v2"
)

func main() {
	app, err := appcli.NewApp(openapi.Spec)
	if err != nil {
		fmt.Fprintf(os.Stderr, "fatal: %v\n", err)
		os.Exit(1)
	}
	app.Commands = append(app.Commands, appcli.MCPCommand())
	app.Commands = append(app.Commands, &cli.Command{
		Name:    "tui",
		Aliases: []string{"i"},
		Usage:   "Start interactive TUI mode with config selector",
		Action: func(c *cli.Context) error {
			return tui.RunTUI(c.String("config"))
		},
	})
	if err := app.Run(os.Args); err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
}
