// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package fleetup

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"reflect"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"gopkg.in/yaml.v3"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/lock"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/readiness"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/run"
)

func parse(t *testing.T, text string) (Settings, error) {
	t.Helper()
	var n yaml.Node
	if err := yaml.Unmarshal([]byte(text), &n); err != nil {
		t.Fatal(err)
	}
	return ParseSettings(n)
}

// TestParseSettingsDefaultsAndStrictness proves the defaults, that every key reads (the store's included), and that
// an unknown key or an out-of-range value at any depth is refused without repeating the YAML.
func TestParseSettingsDefaultsAndStrictness(t *testing.T) {
	s, err := parse(t, "{}")
	if err != nil || !s.CheckStore || s.Mat.Selector != "app.kubernetes.io/component=machine-a-tron" || s.Mat.ControlPort != 1266 {
		t.Fatalf("defaults = %+v, %v", s, err)
	}
	s, err = parse(t, `
expectedRelease: v1
mat: {selector: app=mat, controlPort: 2000}
checkStore: false
expected: {racks: 1, hosts: 2, switches: 0, powerShelves: 4, dpus: 5}
bmc: {port: 8443, concurrency: 8, timeoutSeconds: 3, tries: 1}
nicoApi: {certDir: /certs, serverName: nico, spiffe: {trustDomain: td, serviceBasePaths: [/a/], machineBasePath: /m/}}
store: {url: "http://store:9090", bearerTokenFile: /t, caFile: /ca, headers: {X-Scope-OrgID: t1}, ingestionDelaySeconds: 30}
`)
	switch {
	case err != nil:
		t.Fatal(err)
	case s.ExpectedRelease != "v1" || s.Mat.Selector != "app=mat" || s.Mat.ControlPort != 2000 || s.CheckStore:
		t.Errorf("top level = %+v", s)
	case s.Expected != (readiness.ExpectedCounts{Racks: 1, Hosts: 2, Switches: 0, PowerShelves: 4, DPUs: 5}) || s.Bmc.Port != 8443 || s.Bmc.Tries != 1:
		t.Errorf("counts or bmc = %+v %+v", s.Expected, s.Bmc)
	case s.NicoAPI.CertDir != "/certs" || s.NicoAPI.Spiffe.ServiceBasePaths[0] != "/a/" || s.NicoAPI.ServerName != "nico":
		t.Errorf("nicoApi = %+v", s.NicoAPI)
	case s.Store.URL != "http://store:9090" || *s.Store.BearerTokenFile != "/t" || *s.Store.CAFile != "/ca" ||
		s.Store.Headers["X-Scope-OrgID"] != "t1" || s.Store.IngestionDelaySeconds != 30:
		t.Errorf("store = %v", s.Store)
	}
	for _, bad := range []string{"unknown: 1", "store: {token: secret-value}", "mat: {controlPort: 70000}", "bmc: {tries: -1}", "checkStore: [1]",
		"expected: {hosts: 1.9}", "store: {ingestionDelaySeconds: 30.5}"} {
		if _, err := parse(t, bad); err == nil || err.Error() != "config settings: unknown key, wrong value type or bad YAML" {
			t.Errorf("%q: err = %v, want the fixed refusal", bad, err)
		}
	}
}

type fixedClock struct{ at time.Time }

func (c fixedClock) Now() time.Time     { return c.at }
func (c fixedClock) Instant() time.Time { return c.at }

// TestRunVerdictReasonsAndDeadline proves the verdict, reasons and detail come from the report, a pass's detail is in
// the detail but no reason, the readiness deadline keeps a margin of a tenth of the window at most 5 s, the checks read
// the run clock, and a cancel aborts.
func TestRunVerdictReasonsAndDeadline(t *testing.T) {
	at := time.Date(2026, 11, 5, 10, 0, 0, 0, time.UTC)
	note := readiness.Check{Name: "controlPlanePods", OK: true, Detail: "nico/nico-api-0: 3 restarts"}
	for _, tc := range []struct {
		window  uint64
		checks  []readiness.Check
		verdict result.Verdict
		reasons []string
		margin  time.Duration
	}{
		{300, []readiness.Check{{Name: "bmc", OK: true}, note}, result.Go, []string{}, 5 * time.Second},
		{20, []readiness.Check{{Name: "bmc", Detail: "1 of 1 BMCs failed"}, note}, result.NotReady, []string{"bmc: 1 of 1 BMCs failed"}, 2 * time.Second},
		{30, []readiness.Check{{Name: "bmc", OK: true}}, result.Go, []string{}, 3 * time.Second},
	} {
		var deadline time.Time
		var now func() time.Time
		test := New(&config.Config{Timeouts: config.Timeouts{ReadinessSeconds: tc.window}}, readiness.Deps{})
		test.check = func(_ context.Context, d readiness.Deps, until time.Time) readiness.Report {
			deadline, now = until, d.Now
			return readiness.Report{Checks: tc.checks}
		}
		out, err := test.Run(run.RunCtx{Ctx: context.Background(), Clock: fixedClock{at}, ProbeReadEnd: at.Add(time.Hour)}, nil)
		want := readiness.Report{Checks: tc.checks}.Detail()
		if err != nil || out.Verdict != tc.verdict || out.ExitCode != result.ExitCode(tc.verdict) ||
			!reflect.DeepEqual(out.Reasons, tc.reasons) || string(out.Detail) != string(want) {
			t.Errorf("window %d: %+v, %v; want %s %v %s", tc.window, out, err, tc.verdict, tc.reasons, want)
		}
		if wantAt := at.Add(time.Duration(tc.window)*time.Second - tc.margin); !deadline.Equal(wantAt) || now == nil || !now().Equal(at) {
			t.Errorf("window %d: deadline %v, want %v; checks must read the run clock", tc.window, deadline, wantAt)
		}
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	test := New(&config.Config{Timeouts: config.Timeouts{ReadinessSeconds: 60}}, readiness.Deps{})
	test.check = func(context.Context, readiness.Deps, time.Time) readiness.Report { return readiness.Report{} }
	if out, err := test.Run(run.RunCtx{Ctx: ctx, Clock: fixedClock{at}}, nil); err != context.Canceled || out.Verdict != "" {
		t.Errorf("cancelled run = %+v, %v; want the cancel", out, err)
	}
	if test.Name() != "fleet-up" || test.LockKind() != lock.HolderFleetUp || test.Writes() {
		t.Error("fleet-up is named fleet-up, holds the fleet-up lock and writes nothing")
	}
}

// TestRunReadinessEndsAtTheLastRead proves a setup that leaves less than the readiness window before the run's last
// read cuts the check there, so a check that waits for its deadline ends in a notReady reason, not an abort at the
// evidence end.
func TestRunReadinessEndsAtTheLastRead(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		start := time.Now()
		evidenceEnd, cancel := context.WithDeadline(context.Background(), start.Add(100*time.Second))
		defer cancel()
		test := New(&config.Config{Timeouts: config.Timeouts{ReadinessSeconds: 300}}, readiness.Deps{})
		test.check = func(ctx context.Context, _ readiness.Deps, until time.Time) readiness.Report {
			select {
			case <-ctx.Done():
				return readiness.Report{}
			case <-time.After(time.Until(until)):
				return readiness.Report{Checks: []readiness.Check{{Name: "bmc", Detail: "not finished before the readiness deadline"}}}
			}
		}
		out, err := test.Run(run.RunCtx{Ctx: evidenceEnd, Clock: run.RealClock{}, ProbeReadEnd: start.Add(40 * time.Second)}, nil)
		if err != nil || out.Verdict != result.NotReady || len(out.Reasons) != 1 {
			t.Errorf("outcome = %+v, %v; want notReady with its reason, not an abort", out, err)
		}
	})
}

// TestProductionBuildsTheStoreFromTheProbe proves the store client is built only when checked, in the run, from the
// metadata the probe serves, with age bounds of two scrape or host reader intervals plus the ingestion delay, and
// that a probe that cannot serve it fails the store client, not the wiring.
func TestProductionBuildsTheStoreFromTheProbe(t *testing.T) {
	metrics := `{"nicoApi":null,"rest":null,"restSite":null,"metrics":{"contractVersion":1,"selector":{"job":"p"},` +
		`"scrapeIntervalSeconds":5,"readerIntervalsSeconds":{"host":120,"expected":30},"processStartSeconds":1}}`
	for _, tc := range []struct {
		name, reply   string
		code          int
		check         bool
		err           string
		scrape, sweep time.Duration
	}{
		{"not checked", "", 500, false, "", 0, 0},
		{"checked", metrics, 200, true, "", 40 * time.Second, 270 * time.Second},
		{"a reader interval of centuries", strings.Replace(metrics, `"host":120`, `"host":9e9`, 1), 200, true, "", 40 * time.Second, 48*time.Hour + 30*time.Second},
		{"probe down", "", 503, true, "probe metrics metadata: probe: noCompleteSweep", 0, 0},
		{"no metrics block", `{"nicoApi":null,"rest":null,"restSite":null}`, 200, true, "the probe declares no metrics store", 0, 0},
	} {
		t.Run(tc.name, func(t *testing.T) {
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				w.WriteHeader(tc.code)
				_, _ = io.WriteString(w, tc.reply)
			}))
			defer srv.Close()
			s, _ := parse(t, "{expectedRelease: v1, expected: {hosts: 2}, store: {url: 'http://store:9090', ingestionDelaySeconds: 30}}")
			s.CheckStore = tc.check
			cfg := &config.Config{Namespace: "mat", Probe: config.Probe{URL: srv.URL, RequestTimeoutSeconds: 5},
				ControlPlane: []config.PodSelector{{Namespace: "nico", LabelSelector: "app=nico-api"}}}
			d, err := Production(cfg, s, nil)
			want := readiness.Settings{Namespace: "mat", MatSelector: s.Mat.Selector, MatPort: 1266, Expected: s.Expected, Release: "v1",
				ControlPlane: cfg.ControlPlane}
			if err != nil || (d.Store != nil) != tc.check || !reflect.DeepEqual(d.Settings, want) ||
				d.Dialer.Address(netip.MustParseAddr("10.0.0.1")) != "10.0.0.1:443" {
				t.Fatalf("deps = %+v, %v; want store %v, settings %+v and BMCs on 443", d, err, tc.check, want)
			}
			if !tc.check {
				return
			}
			st, err := d.Store(context.Background())
			if tc.err == "" && (err != nil || st.Client == nil || st.ScrapeBound != tc.scrape || st.SweepBounds[probeapi.FamilyHost] != tc.sweep ||
				st.SweepBounds[probeapi.FamilyExpected] != 90*time.Second) ||
				tc.err != "" && (err == nil || err.Error() != tc.err) {
				t.Errorf("store = %+v, %v; want bounds %v and %v or error %q", st, err, tc.scrape, tc.sweep, tc.err)
			}
		})
	}
}

// ready fakes every readiness seam but the store as a ready site of one host, every pod Ready in every namespace; its
// BMC is a TLS fake at bmc.
type ready struct{ bmc string }

func (ready) Config(context.Context, probe.ConfigFilter) (probeapi.ConfigResponse, error) {
	return probeapi.ConfigResponse{Metrics: &probeapi.MetricsInfo{ContractVersion: probeapi.ContractVersion}}, nil
}
func (ready) Count(context.Context, probeapi.Filter) (probeapi.CountResponse, error) {
	return probeapi.CountResponse{SweepInfo: probeapi.SweepInfo{SweepAgeSeconds: 1, SweepIntervalSeconds: 10}}, nil
}
func (ready) ListPods(context.Context, string, string) ([]cluster.PodInfo, error) {
	return []cluster.PodInfo{{Name: "mat-0", Ready: true, IP: "10.9.0.1"}}, nil
}
func (ready) Read(context.Context, string) (inventory.Response, error) {
	return inventory.Response{CarbideAPIURL: "https://nico-api:1079", Devices: []inventory.Device{{Kind: inventory.KindMachine, BmcMac: "aa"}}}, nil
}
func (ready) ListMatServices(context.Context) ([]cluster.MatService, error) {
	return []cluster.MatService{{MachineType: "host", ExternalIPs: []netip.Addr{netip.MustParseAddr("10.0.0.1")}}}, nil
}
func (r ready) Address(netip.Addr) string { return r.bmc }

// TestRunRecordsAStoreClientFailureAsNotReady proves a store client that cannot be built is a notReady verdict with
// its reason in the result and the report, not an error that ends the run before it records anything.
func TestRunRecordsAStoreClientFailureAsNotReady(t *testing.T) {
	bmcs := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, `{"@odata.id":"/redfish/v1"}`)
	}))
	defer bmcs.Close()
	const why = "store client: the probe declares no metrics store"
	f := ready{bmcs.Listener.Addr().String()}
	d := readiness.Deps{Probe: f, Pods: f, Records: f, Services: f, Dialer: f,
		Identity: func() (string, error) { return "id", nil },
		Version:  func(context.Context, string) (string, error) { return "v1", nil },
		Store: func(context.Context) (readiness.StoreProbe, error) {
			return readiness.StoreProbe{}, errors.New("the probe declares no metrics store")
		},
		Settings: readiness.Settings{Namespace: "mat", Release: "v1", Expected: readiness.ExpectedCounts{Hosts: 1},
			ControlPlane: []config.PodSelector{{Namespace: "nico", LabelSelector: "app=nico-api"}}}}
	out, err := New(&config.Config{Timeouts: config.Timeouts{ReadinessSeconds: 30}}, d).Run(run.RunCtx{Ctx: context.Background(), Clock: run.RealClock{}, ProbeReadEnd: time.Now().Add(time.Hour)}, nil)
	if err != nil || out.Verdict != result.NotReady || !reflect.DeepEqual(out.Reasons, []string{"metricsStore: " + why}) ||
		!strings.Contains(string(out.Detail), why) {
		t.Errorf("outcome = %+v, %v; want notReady for %q alone, in the report too", out, err, why)
	}
}
