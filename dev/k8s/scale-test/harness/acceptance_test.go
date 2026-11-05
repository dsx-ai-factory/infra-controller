// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//go:build acceptance

// Package harness holds the real-cluster acceptance for the MAT inventory reader and the nico-api client. It is read-only (lists Services and Pods,
// reads the live MAT records, checks them, makes one Version call) and sends no AddExpected write. The write RPCs and TLS fault cases are proven by
// the loopback fixture in internal/forge. It uses only the pod ServiceAccount (in-cluster config), so a missing SA fails rather than reading local
// config; the runner chart runs it as a helm test pod, with the expected current-site counts and release from the environment.
package harness

import (
	"context"
	"os"
	"strconv"
	"strings"
	"testing"
	"time"

	"k8s.io/client-go/rest"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/forge"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
)

// env reads a SCALE_TEST_<key> variable the chart sets, or a fallback.
func env(key, fallback string) string {
	if v := os.Getenv("SCALE_TEST_" + key); v != "" {
		return v
	}
	return fallback
}

// reqUint reads a required count from SCALE_TEST_<key>: a missing or unparsable value fails the acceptance rather than passing with a wrong
// expectation.
func reqUint(t *testing.T, key string) uint64 {
	t.Helper()
	v, err := strconv.ParseUint(os.Getenv("SCALE_TEST_"+key), 10, 64)
	if err != nil {
		t.Fatalf("set SCALE_TEST_%s to the expected current-site count: %v", key, err)
	}
	return v
}

// TestAcceptance reads the live inventory and compares every type count, the write plan totals and order, and the nico-api release against the
// expected current-site values an independent precheck supplies. It sends no write.
func TestAcceptance(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	ns := env("NAMESPACE", "")
	if ns == "" {
		t.Fatal("set SCALE_TEST_NAMESPACE to the target MAT namespace")
	}
	selector := env("SELECTOR", "app.kubernetes.io/component=machine-a-tron")
	want := inventory.TypeCounts{
		RackGroup: reqUint(t, "EXPECTED_RACKS"), Rack: reqUint(t, "EXPECTED_RACKS"),
		Host: reqUint(t, "EXPECTED_HOSTS"), DPU: reqUint(t, "EXPECTED_DPUS"),
		Switch: reqUint(t, "EXPECTED_SWITCHES"), PowerShelf: reqUint(t, "EXPECTED_POWER_SHELVES"),
	}
	wantServices := reqUint(t, "EXPECTED_MIN_SERVICES")
	wantRelease := env("RELEASE", "")
	if wantRelease == "" {
		t.Fatal("set SCALE_TEST_RELEASE to the expected nico-api build")
	}

	cfg, err := rest.InClusterConfig()
	if err != nil {
		t.Fatalf("in-cluster config (the pod ServiceAccount is required): %v", err)
	}
	k, err := cluster.NewFor(cfg, ns)
	if err != nil {
		t.Fatalf("kube client: %v", err)
	}
	services, err := k.ListMatServices(ctx)
	if err != nil {
		t.Fatalf("list MAT services: %v", err)
	}
	if uint64(len(services)) < wantServices || wantServices == 0 {
		t.Errorf("MAT services %d, want at least %d (nonzero)", len(services), wantServices)
	}

	inv := inventory.Read(ctx, k, ns, inventory.NewHTTPReader(), selector, 1266)
	for _, p := range inv.Problems {
		t.Errorf("inventory problem: %s", p) // no join problems allowed
	}
	for _, pod := range inv.Pods {
		if pod.Records.RegisterExpectedMachines {
			t.Errorf("pod %s has registration enabled", pod.Name)
		}
	}
	if inv.N != want {
		t.Errorf("type counts %+v, expected %+v", inv.N, want)
	}
	url, ok := inv.CarbideAPIURL()
	if !ok {
		t.Fatal("the MAT pods disagree on the nico-api URL")
	}
	plan, err := inventory.NewPlan(&inv)
	if err != nil {
		t.Fatalf("write plan: %v", err)
	}
	keys := plan.UnitKeys()
	if uint64(len(keys)) != want.Rack+want.Host+want.Switch+want.PowerShelf {
		t.Errorf("plan unit keys %d, expected rack+host+switch+powerShelf", len(keys))
	}
	for i, key := range keys { // racks are planned before every device key
		isRack := strings.HasPrefix(string(key), "rack/")
		if isRack != (uint64(i) < want.Rack) {
			t.Errorf("unit key %d (%s) is out of plan order", i, key)
		}
	}

	// Mounted identity and a verified Version call. No writes are sent. A null dpuPolicy is valid; nic/ignore preservation is proven by the unit
	// fixture.
	spiffe := forge.SpiffeSettings{
		TrustDomain:      env("TRUST_DOMAIN", "nico.local"),
		ServiceBasePaths: strings.Split(env("SERVICE_BASE_PATHS", "/ns/forge/sa/"), ","),
		MachineBasePath:  env("MACHINE_BASE_PATH", "/machine/"),
	}
	certDir := env("CERT_DIR", forge.DefaultCertDir)
	if _, err := forge.CheckIdentityFiles(certDir+"/tls.crt", certDir+"/tls.key", spiffe); err != nil {
		t.Fatalf("mounted identity: %v", err)
	}
	c, err := forge.Dial(url, forge.Settings{CertDir: certDir, Spiffe: spiffe, ServerName: env("SERVER_NAME", "forge.local")})
	if err != nil {
		t.Fatalf("dial nico-api: %v", err)
	}
	defer func() { _ = c.Close() }()
	version, err := c.Version(ctx)
	if err != nil {
		t.Fatalf("Version call: %v", err)
	}
	if version != wantRelease {
		t.Errorf("nico-api release %q, expected %q", version, wantRelease)
	}
}
