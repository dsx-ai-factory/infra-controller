// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// scale-test runs a NICo scale test against a site and reports its result.
// The test commands arrive in later changes. Today the binary prints its
// version.
package main

import (
	"fmt"
	"io"
	"os"
	"strings"
)

// version is set at build time with -ldflags "-X main.version=<version>".
var version = "dev"

// exitNotRun is the exit code of a run that did not start: a bad command
// line, bad config or a refused lock.
const exitNotRun = 2

const usage = "usage: scale-test version"

func main() {
	os.Exit(run(os.Args[1:], os.Stdout, os.Stderr))
}

func run(args []string, stdout, stderr io.Writer) int {
	switch command := strings.Join(args, " "); command {
	case "version":
		_, _ = fmt.Fprintln(stdout, "scale-test", version)
		return 0
	case "":
	default:
		_, _ = fmt.Fprintf(stderr, "scale-test: unknown command %q\n", command)
	}
	_, _ = fmt.Fprintln(stderr, usage)
	return exitNotRun
}
