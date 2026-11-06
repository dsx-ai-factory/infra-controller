// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//go:build devsite

package fleetup_test

import (
	"context"
	"os"
	"strconv"
	"strings"
	"testing"
	"time"

	"gopkg.in/yaml.v3"
	"k8s.io/client-go/rest"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/fleetup"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/forge"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/readiness"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/run"
)

// TestAgainstARealSite runs every fleet-up readiness check against the live site with the production wiring and needs
// go. It writes nothing and takes no lock. It uses only the pod ServiceAccount and the chart's SCALE_TEST_* values, and
// fails, never skips, when one is unset.
func TestAgainstARealSite(t *testing.T) {
	env := func(key string) string {
		v := os.Getenv("SCALE_TEST_" + key)
		if v == "" {
			t.Fatalf("set SCALE_TEST_%s", key)
		}
		return v
	}
	count := func(key string) uint64 {
		n, err := strconv.ParseUint(env("EXPECTED_"+key), 10, 64)
		if err != nil {
			t.Fatalf("SCALE_TEST_EXPECTED_%s is not a count", key)
		}
		return n
	}
	s, err := fleetup.ParseSettings(yaml.Node{Kind: yaml.MappingNode, Tag: "!!map"})
	if err != nil {
		t.Fatal(err)
	}
	s.ExpectedRelease = env("RELEASE")
	s.Mat.Selector = env("SELECTOR")
	s.Expected = readiness.ExpectedCounts{Racks: count("RACKS"), Hosts: count("HOSTS"), Switches: count("SWITCHES"),
		PowerShelves: count("POWER_SHELVES"), DPUs: count("DPUS")}
	s.NicoAPI.CertDir, s.NicoAPI.ServerName = env("CERT_DIR"), env("SERVER_NAME")
	s.NicoAPI.Spiffe = forge.SpiffeSettings{TrustDomain: env("TRUST_DOMAIN"),
		ServiceBasePaths: strings.Split(env("SERVICE_BASE_PATHS"), ","), MachineBasePath: env("MACHINE_BASE_PATH")}
	s.Store.URL = env("STORE_URL")
	if f := os.Getenv("SCALE_TEST_STORE_TOKEN_FILE"); f != "" {
		s.Store.BearerTokenFile = &f
	}
	if f := os.Getenv("SCALE_TEST_STORE_CA_FILE"); f != "" {
		s.Store.CAFile = &f
	}
	if d := os.Getenv("SCALE_TEST_STORE_INGESTION_DELAY"); d != "" {
		if s.Store.IngestionDelaySeconds, err = strconv.ParseUint(d, 10, 64); err != nil {
			t.Fatal("SCALE_TEST_STORE_INGESTION_DELAY is not whole seconds")
		}
	}
	cfg := &config.Config{Namespace: env("NAMESPACE"), Probe: config.Probe{URL: env("PROBE_URL"), RequestTimeoutSeconds: 10},
		Timeouts: config.Timeouts{ReadinessSeconds: 240}}
	if yaml.Unmarshal([]byte(env("CONTROL_PLANE")), &cfg.ControlPlane) != nil {
		t.Fatal("SCALE_TEST_CONTROL_PLANE is not a list of namespace and labelSelector")
	}

	in, err := rest.InClusterConfig()
	if err != nil {
		t.Fatalf("in-cluster config (the pod ServiceAccount is required): %v", err)
	}
	kube, err := cluster.NewFor(in, cfg.Namespace)
	if err != nil {
		t.Fatalf("kube client: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()
	deps, err := fleetup.Production(cfg, s, kube)
	if err != nil {
		t.Fatalf("production wiring: %v", err)
	}
	out, err := fleetup.New(cfg, deps).Run(run.RunCtx{Ctx: ctx, Clock: run.RealClock{}, ProbeReadEnd: time.Now().Add(5 * time.Minute)}, nil)
	t.Logf("checks: %s", out.Detail)
	if err != nil || out.Verdict != result.Go {
		t.Fatalf("verdict %q (%v), want go: %v", out.Verdict, err, out.Reasons)
	}
}
