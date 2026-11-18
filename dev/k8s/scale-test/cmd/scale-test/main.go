// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// scale-test runs a NICo scale test against a site and reports its result.
// fleet-up finds whether the site is ready; version prints the version.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/signal"
	"strings"
	"syscall"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/fleetup"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/lock"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	runner "github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/run"
)

// version is set at build time with -ldflags "-X main.version=<version>".
var version = "dev"

// exitNotRun is the exit code of a run that did not start: a bad command
// line, bad config or a refused lock.
const exitNotRun = result.ExitNotRun

const usage = "usage: scale-test version | scale-test fleet-up [--config <path>]"

func main() {
	os.Exit(run(os.Args[1:], os.Stdout, os.Stderr))
}

func run(args []string, stdout, stderr io.Writer) int {
	if len(args) > 0 && args[0] == "fleet-up" {
		return fleetUp(args[1:], stdout, stderr)
	}
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

// fleetUp runs fleet-up and prints its verdict and reasons. SIGTERM and SIGINT cancel the run, which still publishes
// its result and releases the site lock. A setup failure before the run exits 2 with its cause on stderr.
func fleetUp(args []string, stdout, stderr io.Writer) int {
	flags := flag.NewFlagSet("fleet-up", flag.ContinueOnError)
	flags.SetOutput(io.Discard)
	path := flags.String("config", "/etc/scale-test/config.yaml", "")
	if err := flags.Parse(args); err != nil || flags.NArg() > 0 {
		_, _ = fmt.Fprintln(stderr, usage)
		return exitNotRun
	}
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGTERM, os.Interrupt)
	defer stop()
	deps, test, err := setUp(ctx, *path)
	if err != nil {
		_, _ = fmt.Fprintln(stderr, "scale-test fleet-up:", err)
		return exitNotRun
	}
	res := runner.Execute(deps, test)
	_, _ = fmt.Fprintln(stdout, res.Verdict)
	for _, r := range res.Reasons {
		_, _ = fmt.Fprintln(stdout, "  "+r)
	}
	return result.ExitCode(res.Verdict)
}

// setUp checks the config and wires the run. The execution ID is the pod name, from SCALE_TEST_EXECUTION_ID.
func setUp(ctx context.Context, path string) (runner.Deps, runner.Test, error) {
	cfg, err := config.Load(path, "fleet-up")
	if err != nil {
		return runner.Deps{}, nil, err
	}
	cfg.ExecutionID = os.Getenv("SCALE_TEST_EXECUTION_ID")
	if err := errors.Join(cfg.CheckExecutionID(), cfg.CheckDryRun(false)); err != nil {
		return runner.Deps{}, nil, err
	}
	s, err := fleetup.ParseSettings(cfg.Settings)
	if err != nil {
		return runner.Deps{}, nil, err
	}
	kube, err := cluster.New(cfg.Namespace)
	if err != nil {
		return runner.Deps{}, nil, err
	}
	d, err := fleetup.Production(cfg, s, kube)
	if err != nil {
		return runner.Deps{}, nil, err
	}
	site := &lock.SiteLock{Cluster: kube, Name: cfg.Site.LockName, Timing: lock.DefaultTiming}
	return runner.Deps{Config: cfg, Lock: site, CM: kube, Outside: ctx}, fleetup.New(cfg, d), nil
}
