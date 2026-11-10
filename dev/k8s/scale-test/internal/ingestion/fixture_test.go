// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"maps"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/run"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
	"golang.org/x/time/rate"
)

type fixtureSite struct {
	*instanceSite
	rows           map[string][]map[string]any
	mode           string
	reads          int
	writes         int
	publish        map[string]any
	control        *run.Control
	providerError  error
	allocationMode string
	configReads    int
	readHook       func(string)
	tenantAt       time.Time
}

func (s *fixtureSite) Fixture(ctx context.Context, kind string) (probeapi.FixtureResponse, error) {
	if kind == "provider" && s.providerError != nil {
		return probeapi.FixtureResponse{}, s.providerError
	}
	if s.mode == "read-deadline" {
		<-ctx.Done()
		return probeapi.FixtureResponse{}, ctx.Err()
	}
	if err := ctx.Err(); err != nil {
		return probeapi.FixtureResponse{}, err
	}
	s.reads++
	if s.readHook != nil {
		s.readHook(kind)
	}
	reply := probeapi.FixtureResponse{Objects: []json.RawMessage{}, Kind: kind, SiteID: instanceUUID(800), Org: "tenant", ProcessStartSeconds: s.process, CallsTotal: uint64(s.reads), ReadStart: jsonx.Time{Time: time.Now().Add(-time.Millisecond)}, ReadAt: jsonx.Time{Time: time.Now()}}
	rows := s.rows[kind]
	if kind == "providerBlocks" && rows == nil {
		rows = s.rows["ipblock"]
	}
	for _, row := range rows {
		raw, _ := json.Marshal(row)
		reply.Objects = append(reply.Objects, raw)
	}
	if s.mode == "epoch" {
		reply.ProcessStartSeconds++
	}
	if s.mode == "site" {
		reply.SiteID = instanceUUID(801)
	}
	if s.mode == "future" {
		reply.ReadAt.Time = time.Now().Add(time.Hour)
	}
	if s.mode == "overflow" {
		reply.CallsTotal = ^uint64(0)
	}
	if s.mode == "stale" {
		reply.ReadStart.Time = time.Now().Add(-time.Minute)
	}
	return reply, nil
}
func (s *fixtureSite) Units(ctx context.Context, q probeapi.Filter, pattern *string, page probe.Page) (probeapi.UnitsResponse, error) {
	if s.mode == "first-instance-sweep" && time.Now().Before(s.tenantAt) && familyFor(q) == probeapi.FamilyRestInstances {
		return probeapi.UnitsResponse{}, &probe.Error{Kind: probe.NoCompleteSweep}
	}
	return s.instanceSite.Units(ctx, q, pattern, page)
}
func (s *fixtureSite) Config(ctx context.Context, q probe.ConfigFilter) (probeapi.ConfigResponse, error) {
	s.mu.Lock()
	s.configReads++
	s.mu.Unlock()
	v, e := s.instanceSite.Config(ctx, q)
	v.Metrics, v.RestSite = nil, nil
	if s.mode == "first-allocation-sweep" && time.Now().Before(s.tenantAt) {
		v.Rest = nil
	}
	switch s.allocationMode {
	case "failed":
		v.Rest.LastAttemptError = new("allocation failed")
	case "missing":
		v.Rest = nil
	case "timeout":
		return probeapi.ConfigResponse{}, context.DeadlineExceeded
	}
	return v, e
}
func (s *fixtureSite) RoundTrip(req *http.Request) (*http.Response, error) {
	s.writes++
	if s.mode == "first-401" {
		return &http.Response{StatusCode: 401, Body: io.NopCloser(strings.NewReader("refused"))}, nil
	}
	if s.publish != nil {
		s.rows["vpc"] = []map[string]any{s.publish}
	}
	if s.mode == "transport" {
		return nil, errors.New("credential-secret https://private.example")
	}
	if s.mode == "deadline" {
		<-req.Context().Done()
		return nil, req.Context().Err()
	}
	if s.mode == "late" {
		s.rows["vpc"] = []map[string]any{{"id": instanceUUID(810)}}
		return &http.Response{StatusCode: 500, Body: io.NopCloser(strings.NewReader("secret"))}, nil
	}
	body := []map[string]string{{"id": instanceUUID(810)}}
	raw, _ := json.Marshal(body[0])
	return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(strings.NewReader(string(raw)))}, nil
}
func fixtureRig(t *testing.T) (*fixtureRun, *fixtureSite, *rig) {
	t.Helper()
	site := &fixtureSite{instanceSite: instanceFixture(6), rows: map[string][]map[string]any{}}
	test, r, c := instanceCommand(t, site.instanceSite, "instance-create", "fixture-run", t.TempDir(), 5)
	site.control = c
	test.s.Rest.Org = "tenant"
	test.s.Fixture = FixtureSettings{Name: "fixture", SiteID: instanceUUID(800), SetupSeconds: 3600, IPBlockCIDR: "10.210.0.0/16", FabricPrefixCIDRs: []string{"10.210.0.0/16"}, MachineCapabilities: []probeapi.MachineCapability{{Type: "GPU", Name: "gpu", Count: new(uint64(4))}}}
	test.s.sidecar = site
	test.rest.client = &http.Client{Transport: site}
	test.rest.settings = test.s
	in := &instanceRun{t: test, w: r.w, ctx: c.Context(), end: time.Now().Add(time.Hour), fixtureRESTClosed: true, d: instanceDetail{Evidence: map[string]any{}}}
	in.health.Store(&instanceHealth{at: time.Now()})
	in.store = &instanceMetrics{}
	in.epoch = site.process
	in.cp = capture(in.ctx, test.deps.Pods, test.cfg.ControlPlane, true).Pods
	in.mat = capture(in.ctx, test.deps.Pods, []config.PodSelector{{Namespace: test.cfg.Namespace, LabelSelector: test.s.Mat.Selector}}).Pods
	in.latest = &instanceSnapshot{meta: map[probeapi.Family]probeapi.SweepInfo{probeapi.FamilyHost: site.meta(probeapi.FamilyHost)}}
	f := &fixtureRun{in: in, p: site, end: in.end, previous: map[string]fixtureRecord{}, provider: instanceUUID(802), tenant: instanceUUID(901), report: map[string]any{}, loaded: true, ledger: fixtureLedger{Name: test.s.Fixture.Name, Site: test.s.Fixture.SiteID, Org: test.s.Rest.Org, Provider: test.s.Rest.ProviderOrg, Origin: test.s.Rest.URL, RunID: test.journalID(), TenantClient: test.s.Rest.Auth.TenantClientID, Records: []fixtureRecord{}, Cohort: map[string]string{}}}
	return f, site, r
}
func TestFixtureSafety(t *testing.T) {
	for _, mode := range []string{"borrowed", "incompatible", "duplicate", "site", "epoch", "stale", "late", "lost-lock", "persist", "deadline", "read-deadline", "cancel", "stop", "dry", "created", "first-401", "prior-401", "dispatch-freshness", "health-stop", "replacement", "token-expiry", "recreate", "token-response-delay", "accepted-wait"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				f, s, r := fixtureRig(t)
				row := map[string]any{"id": instanceUUID(810), "siteId": f.in.t.s.Fixture.SiteID, "tenantId": f.tenant, "name": "fixture-vpc", "status": "Ready"}
				body := map[string]any{"name": "fixture-vpc", "siteId": f.in.t.s.Fixture.SiteID}
				s.rows["vpc"] = []map[string]any{row}
				switch mode {
				case "recreate":
					s.rows["vpc"], s.publish = nil, row
					f.previous["vpc"] = fixtureRecord{ID: instanceUUID(811), Owned: true, Proof: 7}
				case "replacement":
					f.previous["vpc"] = fixtureRecord{ID: text(row, "id"), Owned: true, Proof: 7}
					reads := 0
					s.readHook = func(kind string) {
						if kind == "vpc" {
							reads++
							if reads == 2 {
								row["id"] = instanceUUID(811)
							}
						}
					}
				case "incompatible":
					row["name"] = "foreign"
				case "duplicate":
					s.rows["vpc"] = append(s.rows["vpc"], row)
				case "site", "stale":
					s.mode = mode
				case "epoch":
					f.epoch = s.process
					s.mode = mode
				case "late":
					s.rows["vpc"] = nil
					s.mode = mode
				case "lost-lock":
					s.rows["vpc"] = nil
					r.gate.lost.Store(true)
				case "persist":
					s.rows["vpc"] = nil
					r.j.Inject(func(*os.File, []byte) error { return errors.New("disk failed") })
				case "cancel":
					s.control.Cancel()
				case "stop":
					s.rows["vpc"] = nil
					s.control.RequestStop()
				case "read-deadline":
					s.mode = mode
					f.end = time.Now().Add(time.Second)
				case "deadline":
					s.rows["vpc"] = nil
					s.mode = mode
					f.end = time.Now().Add(time.Second)
				case "dry":
					s.rows["vpc"] = nil
					f.in.rc.DryRun = true
				case "first-401", "prior-401":
					s.rows["vpc"], s.mode = nil, "first-401"
					if mode == "prior-401" {
						if err := f.in.w.Pending(f.in.ctx, true); err != nil {
							t.Fatal(err)
						}
					}
				case "dispatch-freshness", "token-expiry", "token-response-delay":
					s.rows["vpc"] = nil
					delay := 31 * time.Second
					if mode == "token-expiry" {
						delay = 36 * time.Second
						f.in.latest.meta[probeapi.FamilyHost] = probeapi.SweepInfo{SweepStart: jsonx.Time{Time: time.Now()}}
						f.in.t.rest.settings.Rest.Auth.TokenURL = "https://identity.example/token"
						f.in.t.rest.auth.tokens = map[bool]oauthToken{false: {"token", time.Now().Add(35 * time.Second)}}
					}
					if mode == "token-response-delay" {
						delay = 24 * time.Second
						f.in.latest.meta[probeapi.FamilyHost] = probeapi.SweepInfo{SweepStart: jsonx.Time{Time: time.Now()}}
						rest := f.in.t.rest
						rest.settings.Rest.Auth = fixtureAuth{TokenURL: "https://identity.example/token", CredentialsDir: t.TempDir()}
						if err := writeIdentity(rest.settings.Rest.Auth.CredentialsDir, "tenant", clientIdentity{"tenant", "secret"}); err != nil {
							t.Fatal(err)
						}
						rest.authClient = &http.Client{Transport: labelRetryTransport(func(*http.Request) (*http.Response, error) {
							time.Sleep(9 * time.Second)
							return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"access_token":"token","expires_in":31}`))}, nil
						})}
					}
					delayed := false
					f.in.w = writer.New(r.j, labelRetryGate(func() error {
						if r.j.Counts().Attempts > 0 && !delayed {
							delayed = true
							time.Sleep(delay)
						}
						return r.gate.Check()
					}), s.control.Stopper(), f.end)
				case "health-stop":
					s.rows["vpc"] = nil
					f.in.healthStop = make(chan struct{})
					close(f.in.healthStop)
				case "created", "accepted-wait":
					s.rows["vpc"] = nil
				}
				if mode == "created" || mode == "accepted-wait" {
					s.publish = row
				}
				if mode == "accepted-wait" {
					if e := f.record(fixtureRecord{Kind: "os", ID: instanceUUID(820), Path: "operating-system/" + instanceUUID(820), Owned: true, Proof: 7, Spec: map[string]any{"name": "fixture-os"}}); e != nil {
						t.Fatal(e)
					}
					f.ledger.Records = nil
					if e := f.load(); e != nil {
						t.Fatal(e)
					}
					row["status"] = "Error"
				}
				_, err := f.ensure("vpc", body, "Ready")
				if mode == "accepted-wait" {
					next := *f
					next.ledger.Records, next.previous, next.imported, next.loaded = nil, nil, nil, false
					if e := next.load(); e != nil || !next.previous["vpc"].Owned || next.previous["vpc"].ID != text(row, "id") || next.previous["vpc"].Proof == 0 || !next.previous["os"].Owned {
						t.Fatalf("accepted creation lost ownership after wait failure: %v", e)
					}
				}
				success := mode == "borrowed" || mode == "dry" || mode == "created" || mode == "recreate"
				if (err == nil) != success {
					t.Fatalf("ensure %s: %v", mode, err)
				}
				if mode == "late" && (s.writes != 1 || s.reads < 2 || !r.j.Unsettled()) {
					t.Fatal("late server failure replayed or lost pending/read-back")
				}
				if mode == "lost-lock" || mode == "persist" || mode == "dry" || mode == "borrowed" || mode == "incompatible" || mode == "cancel" || mode == "stop" || mode == "read-deadline" {
					if s.writes != 0 {
						t.Fatal("unexpected mutation")
					}
				}
				if mode == "first-401" || mode == "prior-401" || mode == "dispatch-freshness" || mode == "health-stop" || mode == "token-expiry" || mode == "token-response-delay" {
					expected := 0
					if mode == "first-401" || mode == "prior-401" {
						expected = 1
					}
					if s.writes != expected || r.j.Unsettled() != (mode == "prior-401") {
						t.Fatalf("fixture dispatch/pending boundary: writes=%d pending=%t", s.writes, r.j.Unsettled())
					}
				}
				if mode == "borrowed" && (len(f.ledger.Records) != 1 || f.ledger.Records[0].Owned) {
					t.Fatal("borrowed object adopted")
				}
				if (mode == "created" || mode == "recreate") && (len(f.ledger.Records) != 1 || !f.ledger.Records[0].Owned || f.ledger.Records[0].Proof == 0) {
					t.Fatal("creation lost durable ownership")
				}
				if mode == "deadline" && time.Since(f.end) > time.Second {
					t.Fatal("write escaped setup deadline")
				}
			})
		})
	}
}
func TestFixtureTokens(t *testing.T) {
	for _, mode := range []string{"renew", "refused", "oversized", "cancel", "no-retry", "transient", "exhausted", "cancel-retry", "throttled", "transport", "timeout"} {
		t.Run(mode, func(t *testing.T) {
			f, _, _ := fixtureRig(t)
			r := f.in.t.rest
			calls := 0
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			server := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
				calls++
				if req.Host == "" || req.ParseForm() != nil || req.Form.Get("client_id") != "tenant-client" {
					t.Error("canonical client credentials missing")
				}
				if mode == "cancel-retry" {
					time.AfterFunc(50*time.Millisecond, cancel)
					w.WriteHeader(503)
					return
				}
				if mode == "exhausted" || (mode == "transient" || mode == "throttled") && calls == 1 {
					status := 503
					if mode == "throttled" {
						status = 429
					}
					w.WriteHeader(status)
					return
				}
				if mode == "refused" {
					w.WriteHeader(401)
					_, _ = io.WriteString(w, "credential-secret")
					return
				}
				if mode == "oversized" {
					_, _ = io.WriteString(w, strings.Repeat("x", (1<<20)+1))
					return
				}
				_, _ = io.WriteString(w, `{"access_token":"test-token","expires_in":300}`)
			}))
			defer server.Close()
			r.settings.Rest.Auth = fixtureAuth{TokenURL: server.URL, CredentialsDir: t.TempDir()}
			r.authClient = server.Client()
			if mode == "transport" || mode == "timeout" {
				transport := r.authClient.Transport
				r.authClient.Transport = labelRetryTransport(func(req *http.Request) (*http.Response, error) {
					if calls == 0 {
						calls++
						return nil, &net.DNSError{IsTimeout: mode == "timeout", Err: "credential-secret"}
					}
					return transport.RoundTrip(req)
				})
			}
			if err := os.WriteFile(filepath.Join(r.settings.Rest.Auth.CredentialsDir, "tenant.new"), []byte("interrupted credential write"), 0600); err != nil {
				t.Fatal(err)
			}
			if err := writeIdentity(r.settings.Rest.Auth.CredentialsDir, "tenant", clientIdentity{"tenant-client", "credential-secret"}); err != nil {
				t.Fatal(err)
			}
			info, _ := os.Stat(filepath.Join(r.settings.Rest.Auth.CredentialsDir, "tenant.json"))
			if info.Mode().Perm() != 0600 {
				t.Fatal("credential mode")
			}
			if mode == "cancel" {
				cancel()
			}
			started := time.Now()
			err := r.authorize(ctx, false)
			if mode == "cancel-retry" && (!errors.Is(err, context.Canceled) || calls != 1 || time.Since(started) >= 500*time.Millisecond) {
				t.Fatalf("cancelled retry: calls=%d err=%v", calls, err)
			}
			if (mode == "refused" || mode == "oversized") && calls != 1 || mode == "exhausted" && calls != 3 {
				t.Fatalf("token attempt count=%d", calls)
			}
			if mode == "refused" || mode == "oversized" || mode == "cancel" || mode == "exhausted" || mode == "cancel-retry" {
				if err == nil || strings.Contains(err.Error(), "credential-secret") || strings.Contains(err.Error(), server.URL) {
					t.Fatalf("unsafe auth failure: %v", err)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			expected := 1
			if mode == "transient" || mode == "throttled" || mode == "transport" || mode == "timeout" {
				expected = 2
			}
			if err = r.authorize(ctx, false); err != nil || calls != expected {
				t.Fatal("cached token not reused")
			}
			if mode == "renew" {
				r.auth.tokens[false] = oauthToken{"old", time.Now().Add(20 * time.Second)}
				if err = r.authorize(ctx, false); err != nil || calls != 2 {
					t.Fatal("expiry not renewed")
				}
			}
			if mode == "no-retry" {
				r.client = &http.Client{Transport: labelRetryTransport(func(*http.Request) (*http.Response, error) {
					return &http.Response{StatusCode: 401, Body: io.NopCloser(strings.NewReader("secret"))}, nil
				})}
				out := r.attempt(ctx, http.MethodPost, "instance/batch", nil, 1)
				if out.Rejected == nil || calls != 1 {
					t.Fatal("write refusal refreshed or retried")
				}
			}
		})
	}
}

func TestFixtureContracts(t *testing.T) {
	f, s, _ := fixtureRig(t)
	s.rows["provider"] = []map[string]any{{"id": f.provider}}
	settings := f.in.t.s
	settings.Rest.Auth.TokenURL = "https://identity.example/token"
	settings.Rest.TokenFile = "/unavailable-before-claim"
	client, err := newREST(settings)
	if err != nil || client.client.Timeout != 300*time.Second {
		t.Fatalf("setup write bound or premature credential read: %v", err)
	}
	for _, raw := range []string{"http://identity.example/token", "https://user:password@identity.example/token", "https://identity.example/token?q=1"} {
		settings := f.in.t.s
		settings.Rest.Auth = fixtureAuth{TokenURL: raw, ProviderDirectory: "provider", TenantDirectory: "tenant", TenantClientID: "client", CredentialsDir: "memory"}
		settings.Fixture.AllocationPrefixLength = 18
		settings.Fixture.SubnetPrefixLength = 18
		valid := settings
		valid.Rest.Auth.TokenURL = "https://identity.example/token"
		if err := validateFixture(valid); err != nil {
			t.Fatalf("valid fixture: %v", err)
		}
		missingTenant := valid
		missingTenant.Rest.Auth.TenantDirectory = ""
		if validateFixture(missingTenant) == nil {
			t.Fatal("fixture accepted missing tenant credentials")
		}
		if validateFixture(settings) == nil {
			t.Fatal("unsafe identity URL accepted")
		}
	}
	if fabricPrerequisite(FixtureSettings{IPBlockCIDR: "10.210.0.0/16"}) == nil {
		t.Fatal("missing fabric attestation accepted")
	}
	have := []probeapi.MachineCapability{{Type: "GPU", Name: "gpu"}}
	if capabilitiesMatch(have, f.in.t.s.Fixture.MachineCapabilities) {
		t.Fatal("unknown capability count treated as zero or known")
	}
	have[0].Count = new(uint64(4))
	if !capabilitiesMatch(have, f.in.t.s.Fixture.MachineCapabilities) {
		t.Fatal("compatible capability rejected")
	}
	old := fixtureLedger{Name: "fixture", Site: f.in.t.s.Fixture.SiteID, Org: "tenant", Provider: f.in.t.s.Rest.ProviderOrg, Origin: f.in.t.s.Rest.URL, TenantClient: f.in.t.s.Rest.Auth.TenantClientID}
	for _, bad := range []bool{false, true} {
		old.Records = []fixtureRecord{{Kind: "vpc", ID: instanceUUID(810), Path: "vpc/" + instanceUUID(810), Owned: true, Proof: 1}}
		if bad {
			old.Records[0].Path = "other/" + instanceUUID(810)
		}
		if (f.acceptLedger(old) == nil) == bad {
			t.Fatal("ledger canonical path validation")
		}
	}
	f.in.rc.DryRun = true
	if err := f.finishSetup(nil); err != nil {
		t.Fatal(err)
	}
	if f.report["setup"].(map[string]any)["verdict"] != "planned" {
		t.Fatal("dry setup reported passed")
	}
	if err := f.finishSetup(errors.New("failed")); err == nil {
		t.Fatal("setup failure discarded")
	}
}

func TestFixtureDryRunFreezesBeforePlanning(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		f, s, r := fixtureRig(t)
		test := f.in.t
		test.s.preparing = true
		dir := fixtureCredentials(t, "provider-client")
		test.s.Rest.Auth = fixtureAuth{ProviderDirectory: dir, CredentialsDir: t.TempDir()}
		s.rows["provider"] = []map[string]any{{"id": f.provider}}
		s.rows["type"] = []map[string]any{{"id": instanceUUID(900), "siteId": test.s.Fixture.SiteID, "infrastructureProviderId": f.provider}}
		for id, u := range s.state[probeapi.FamilyRestMachines] {
			u.RestMachine.MachineCapabilities = test.s.Fixture.MachineCapabilities
			s.state[probeapi.FamilyRestMachines][id] = u
		}
		c := run.NewControl()
		t.Cleanup(c.Cancel)
		out, err := runInstance(test, r, c, true)
		if err != nil || out.Verdict != result.Go || s.writes != 0 || r.j.Unsettled() {
			t.Fatalf("dry fixture verdict=%s writes=%d err=%v", out.Verdict, s.writes, err)
		}
		var plan instancePlan
		if err = fixtureFile(test.cfg.Journal.Dir, test.cfg.RunID+".fixture-cohort.json", &plan, ""); err != nil || len(plan.Cohort) != 5 {
			t.Fatalf("frozen cohort: %v", err)
		}
	})
}

func TestFixtureCreateDeleteOwnedAndBorrowedResources(t *testing.T) {
	for _, mode := range []string{"background", "fresh-account", "unstable-selected", "rack", "dpu", "lifecycle-assigned-ready", "lifecycle-failure", "all-instances-gone", "missing-candidate", "fabric-root", "fallback-root", "partial-overlap", "partial-rerun", "unattested-child", "delayed-child", "delayed-id", "delayed-parent", "delayed-prefix", "delayed-tenant", "delayed-site", "delayed-status", "first-allocation-sweep", "first-instance-sweep", "borrowed-invited", "borrowed-unlinked", "borrowed-disabled", "account-link-id", "account-link-scope", "account-accept-id", "account-accept-scope", "account-capability-scope", "cleanup-config-present", "cleanup-config-missing", "cleanup-retained-error"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				f, s, r := fixtureRig(t)
				test := f.in.t
				test.s.preparing = true
				if strings.HasPrefix(mode, "first-") {
					s.mode, s.tenantAt = mode, time.Now().Add(120*time.Second)
				}
				if strings.HasPrefix(mode, "lifecycle") {
					test.cfg.Command = "instance-lifecycle"
					test.s.Fixture.Teardown = true
					if mode == "lifecycle-failure" {
						s.instanceSite.mode = "lost"
					}
				}
				for id, u := range s.state[probeapi.FamilyRestMachines] {
					u.RestMachine.MachineCapabilities = test.s.Fixture.MachineCapabilities
					s.state[probeapi.FamilyRestMachines][id] = u
				}
				// Existing compatible resources are borrowed, including a larger reservation.
				for i, kind := range []string{"account", "type", "compute", "ipblock", "ipallocation", "vpc", "subnet", "os", "blocks"} {
					s.rows[kind] = []map[string]any{{"id": instanceUUID(820 + i), "name": "fixture-" + kind, "siteId": test.s.Fixture.SiteID, "tenantId": instanceUUID(901), "infrastructureProviderId": f.provider, "status": "Ready"}}
				}
				s.rows["provider"] = []map[string]any{{"id": f.provider}}
				s.rows["account"][0]["tenantOrg"] = "tenant"
				s.rows["account"][0]["targetedInstanceCreation"] = true
				delete(s.rows["account"][0], "name")
				s.rows["type"][0]["id"] = instanceUUID(900)
				s.rows["type"][0]["machineCapabilities"] = test.s.Fixture.MachineCapabilities
				constraints := func(kind, id string, n int) []any {
					return []any{map[string]any{"id": instanceUUID(840), "resourceType": kind, "resourceTypeId": id, "constraintType": "Reserved", "constraintValue": n, "derivedResourceId": instanceUUID(828)}}
				}
				s.rows["compute"][0]["status"] = "Registered"
				s.rows["compute"][0]["allocationConstraints"] = constraints("InstanceType", instanceUUID(900), 100)
				for k, v := range map[string]any{"prefix": "10.210.0.0", "prefixLength": 16, "routingType": "DatacenterOnly", "protocolVersion": "IPv4"} {
					s.rows["ipblock"][0][k] = v
				}
				s.rows["ipallocation"][0]["status"] = "Registered"
				s.rows["ipallocation"][0]["allocationConstraints"] = constraints("IPBlock", instanceUUID(823), 18)
				for k, v := range map[string]any{"prefix": "10.210.0.0", "prefixLength": 18} {
					s.rows["blocks"][0][k] = v
				}
				delete(s.rows["subnet"][0], "tenantId")
				s.rows["vpc"][0]["networkVirtualizationType"] = "ETHERNET_VIRTUALIZER"
				for k, v := range map[string]any{"vpcId": instanceUUID(825), "ipv4BlockId": instanceUUID(828), "prefixLength": 18} {
					s.rows["subnet"][0][k] = v
				}
				script, err := readScript(test.s.Instance.IPXEFile)
				if err != nil {
					t.Fatal(err)
				}
				s.rows["os"][0]["ipxeScriptHash"] = digest([]byte(script))
				s.rows["os"][0]["phoneHomeEnabled"] = false
				test.s.Fixture.AllocationPrefixLength = 18
				test.s.Fixture.SubnetPrefixLength = 18
				root, allocation := s.rows["ipblock"][0], s.rows["ipallocation"][0]
				if mode == "fabric-root" || mode == "fallback-root" || mode == "partial-overlap" || mode == "unattested-child" {
					s.rows["ipblock"], s.rows["ipallocation"] = nil, nil
					delete(root, "tenantId")
					if mode != "fallback-root" {
						root["name"] = "site-fabric-ipv4"
						s.rows["providerBlocks"] = []map[string]any{root}
					}
					if mode == "partial-overlap" {
						root["prefixLength"] = 20
					} else {
						test.s.Fixture.IPBlockCIDR = "10.210.64.0/20"
						test.s.Fixture.AllocationPrefixLength, test.s.Fixture.SubnetPrefixLength = 22, 22
						allocation["allocationConstraints"] = constraints("IPBlock", text(root, "id"), 22)
						s.rows["blocks"][0]["prefixLength"], s.rows["subnet"][0]["prefixLength"] = 22, 22
						if mode == "fallback-root" {
							s.rows["blocks"][0]["prefix"] = "10.210.64.0"
						}
					}
				}
				if mode == "unattested-child" {
					test.s.Fixture.FabricPrefixCIDRs = []string{test.s.Fixture.IPBlockCIDR}
				}
				if strings.HasPrefix(mode, "delayed-") {
					c := allocation["allocationConstraints"].([]any)[0].(map[string]any)
					delete(c, "derivedResourceId")
					reads := 0
					s.readHook = func(kind string) {
						if kind != "ipallocation" {
							return
						}
						reads++
						if reads == 3 {
							c["derivedResourceId"] = instanceUUID(828)
							switch mode {
							case "delayed-id":
								allocation["id"] = instanceUUID(999)
							case "delayed-parent":
								c["resourceTypeId"] = instanceUUID(999)
							case "delayed-prefix":
								c["constraintValue"] = 20
							case "delayed-tenant", "delayed-site":
								allocation[strings.TrimPrefix(mode, "delayed-")+"Id"] = instanceUUID(999)
							case "delayed-status":
								allocation["status"] = "Pending"
							}
						}
					}
				}
				dir := fixtureCredentials(t, "tenant-client")
				auth := &http.Client{Transport: labelRetryTransport(func(*http.Request) (*http.Response, error) {
					return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"access_token":"test-token","expires_in":300}`))}, nil
				})}
				test.s.Rest.Auth = fixtureAuth{TokenURL: "https://identity.example/token", ProviderDirectory: dir, TenantDirectory: dir, TenantClientID: "tenant-client", CredentialsDir: t.TempDir()}
				test.rest.settings = test.s
				test.rest.authClient = auth
				labelWrites, accountPatches := 0, 0
				account := s.rows["account"][0]
				if mode == "fresh-account" || mode == "partial-rerun" || strings.HasPrefix(mode, "account-") {
					s.rows["account"] = nil
					account["status"], account["targetedInstanceCreation"] = "Invited", false
					account["siteCapabilities"] = []any{map[string]any{"siteIds": []any{}, "targetedInstanceCreation": false}}
					delete(account, "tenantId")
				}
				if mode == "borrowed-disabled" {
					account["targetedInstanceCreation"] = false
				}
				if mode == "borrowed-invited" {
					account["status"] = "Invited"
				}
				if mode == "borrowed-unlinked" {
					delete(account, "tenantId")
				}
				ownedVPC := s.rows["vpc"][0]
				s.rows["vpc"] = nil
				creates := map[string]int{}
				writes := 0
				test.rest.client.Transport = labelRetryTransport(func(req *http.Request) (*http.Response, error) {
					writes++
					if strings.HasPrefix(mode, "first-") && req.Method == http.MethodPost && strings.HasSuffix(req.URL.Path, "/instance/batch") && time.Now().Before(s.tenantAt) {
						t.Fatal("instance write preceded first tenant sweep")
					}
					if req.Method == http.MethodPost {
						creates[req.URL.Path]++
					}
					if strings.HasSuffix(req.URL.Path, "/ipblock") && req.Method == http.MethodPost {
						var body map[string]any
						_ = json.NewDecoder(req.Body).Decode(&body)
						maps.Copy(root, body)
						s.rows["ipblock"] = []map[string]any{root}
						return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"id":"` + text(root, "id") + `"}`))}, nil
					}
					if strings.HasSuffix(req.URL.Path, "/allocation") && req.Method == http.MethodPost {
						s.rows["ipallocation"] = []map[string]any{allocation}
						return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"id":"` + text(allocation, "id") + `"}`))}, nil
					}
					if req.Method == http.MethodDelete && (strings.HasSuffix(req.URL.Path, "/ipblock/"+text(root, "id")) || strings.HasSuffix(req.URL.Path, "/allocation/"+text(allocation, "id"))) {
						if strings.Contains(req.URL.Path, "/ipblock/") {
							s.rows["ipblock"], s.rows["providerBlocks"] = nil, nil
						} else {
							s.rows["ipallocation"] = nil
						}
						return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(""))}, nil
					}
					if strings.HasSuffix(req.URL.Path, "/tenant/current") {
						if req.Body != nil {
							raw, e := io.ReadAll(req.Body)
							if e != nil || len(raw) != 0 {
								t.Error("tenant discovery GET sent a body")
							}
						}
						account["tenantId"] = instanceUUID(901)
						if mode == "account-link-id" {
							account["id"] = instanceUUID(999)
						}
						if mode == "account-link-scope" {
							account["infrastructureProviderId"] = instanceUUID(999)
						}
					} else if strings.Contains(req.URL.Path, "/tenant/account") {
						s.rows["account"] = []map[string]any{account}
						if req.Method == http.MethodPatch {
							accountPatches++
							var patch map[string]any
							if json.NewDecoder(req.Body).Decode(&patch) != nil {
								t.Fatal("account patch invalid")
							}
							if patch["siteCapabilities"] != nil {
								account["targetedInstanceCreation"] = true
								if mode == "account-capability-scope" {
									account["infrastructureProviderId"] = instanceUUID(999)
								}
							} else {
								account["status"] = "Ready"
								if strings.HasPrefix(mode, "account-accept") {
									account["targetedInstanceCreation"] = true
								}
								if mode == "account-accept-id" {
									account["id"] = instanceUUID(999)
								}
								if mode == "account-accept-scope" {
									account["tenantOrg"] = "foreign"
								}
							}
						}
						if req.Method == http.MethodDelete {
							s.rows["account"] = nil
						}
					} else if req.Method == http.MethodPatch && strings.Contains(req.URL.Path, "/machine/") {
						labelWrites++
					}
					if strings.HasSuffix(req.URL.Path, "/tenant/current") || strings.Contains(req.URL.Path, "/tenant/account") {
						return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"id":"` + instanceUUID(820) + `"}`))}, nil
					}
					if req.Method == http.MethodPost && strings.HasSuffix(req.URL.Path, "/vpc") {
						if mode == "rack" || mode == "dpu" {
							s.frame = -1
						}
						if mode == "rack" {
							s.state[probeapi.FamilyHost]["h0"].Host.RackID = new("changed-rack")
						}
						if mode == "dpu" {
							s.state[probeapi.FamilyHost]["h0"].Host.DpuIDs = []string{"replacement-dpu"}
						}
						s.rows["vpc"] = []map[string]any{ownedVPC}
						if mode == "partial-rerun" && creates[req.URL.Path] == 1 {
							return &http.Response{StatusCode: 409, Body: io.NopCloser(strings.NewReader("conflict"))}, nil
						}
						if mode == "unstable-selected" {
							s.state[probeapi.FamilyHost]["h0"].Host.Allocatable = new(false)
							s.frame = -1
						}
						go func() {
							time.Sleep(20 * time.Second)
							s.mu.Lock()
							defer s.mu.Unlock()
							s.state[probeapi.FamilyHost]["h5"].Host.Allocatable = new(false)
							if mode == "unstable-selected" {
								s.state[probeapi.FamilyHost]["h0"].Host.Allocatable = new(true)
							}
						}()
						return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"id":"` + instanceUUID(825) + `"}`))}, nil
					}
					if req.Method == http.MethodDelete && strings.Contains(req.URL.Path, "/vpc/") {
						s.rows["vpc"] = nil
						return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(""))}, nil
					}
					if req.Method == http.MethodPost && strings.HasSuffix(req.URL.Path, "/instance/batch") && s.rows["account"][0]["targetedInstanceCreation"] != true {
						return &http.Response{StatusCode: 403, Body: io.NopCloser(strings.NewReader("forbidden"))}, nil
					}
					if mode == "cleanup-retained-error" && req.Method == http.MethodDelete {
						return &http.Response{StatusCode: 401, Body: io.NopCloser(strings.NewReader("refused"))}, nil
					}
					reply, err := s.instanceSite.RoundTrip(req)
					s.mu.Lock()
					defer s.mu.Unlock()
					for id, u := range s.state[probeapi.FamilyInstance] {
						u.Instance.Tenant = new("tenant")
						s.state[probeapi.FamilyInstance][id] = u
					}
					return reply, err
				})
				c := run.NewControl()
				defer c.Cancel()
				out, err := runInstance(test, r, c, false)
				if strings.HasPrefix(mode, "account-") {
					expected := 0
					if strings.HasPrefix(mode, "account-accept") {
						expected = 1
					}
					if mode == "account-capability-scope" {
						expected = 2
					}
					if out.Verdict != result.NotReady || accountPatches != expected || labelWrites != 0 || creates["/v2/org/tenant/nico/vpc"] != 0 {
						t.Fatalf("replacement account continued: verdict=%s patches=%d err=%v", out.Verdict, accountPatches, err)
					}
					return
				}

				if strings.HasPrefix(mode, "borrowed-") {
					if out.Verdict != result.NotReady || writes != 0 || !strings.Contains(strings.Join(out.Reasons, " "), "borrowed account") {
						t.Fatalf("borrowed account mutated: %s writes=%d err=%v", out.Verdict, writes, err)
					}
					return
				}
				if strings.HasPrefix(mode, "first-") && time.Now().Before(s.tenantAt) {
					time.Sleep(time.Until(s.tenantAt))
				}
				if mode == "unattested-child" || strings.HasPrefix(mode, "delayed-") && mode != "delayed-child" {
					if out.Verdict != result.NotReady || creates["/v2/org/tenant/nico/vpc"] != 0 || creates["/v2/org/tenant/nico/subnet"] != 0 || creates["/v2/org/tenant/nico/instance/batch"] != 0 || !strings.Contains(strings.Join(out.Reasons, " "), map[bool]string{true: "fabric-prefix prerequisite", false: "IP allocation"}[mode == "unattested-child"]) {
						t.Fatalf("unsafe child: %s creates=%v err=%v reasons=%v", out.Verdict, creates, err, out.Reasons)
					}
					return
				}
				if mode == "partial-overlap" {
					if out.Verdict != result.NotReady || writes != 0 || !strings.Contains(strings.Join(out.Reasons, " "), "partially overlaps") {
						t.Fatalf("overlap: %s writes=%d err=%v reasons=%v", out.Verdict, writes, err, out.Reasons)
					}
					return
				}
				if mode == "partial-rerun" {
					if out.Verdict != result.NotReady {
						t.Fatal("partial fixture did not fail")
					}
					before := maps.Clone(creates)
					retry, rr, cc := instanceCommand(t, s.instanceSite, "instance-create", "fixture-retry", test.cfg.Journal.Dir, 5)
					retry.s, retry.rest.settings, retry.rest.client.Transport, retry.rest.authClient = test.s, test.s, test.rest.client.Transport, auth
					test, r, c = retry, rr, cc
					out, err = runInstance(test, r, c, false)
					for path, count := range creates {
						if !strings.HasSuffix(path, "/instance/batch") && count != before[path] {
							t.Fatalf("fixture recreated on rerun: %s", path)
						}
					}
				}
				if strings.HasPrefix(mode, "lifecycle") {
					want := result.Passed
					if mode == "lifecycle-assigned-ready" {
						var phases map[string]instanceDetail
						if json.Unmarshal(out.Detail, &phases) != nil {
							t.Fatal("lifecycle phases missing")
						}
						owned, e := readOwned(test.cfg.Journal.Dir, phases["create"].Manifest)
						var ledger fixtureLedger
						if e != nil || fixtureFile(test.cfg.Journal.Dir, owned.Fixture["path"], &ledger, owned.Fixture["sha256"]) != nil || ledger.RunID != test.cfg.RunID {
							t.Fatal("fixture proof points outside the shared journal")
						}
					}
					if mode == "lifecycle-failure" {
						want = result.Unresolved
					}
					if err != nil || out.Verdict != want || r.j.Unsettled() != (mode == "lifecycle-failure") {
						t.Fatalf("fixture lifecycle: %s %v %v", out.Verdict, err, out.Reasons)
					}
					if mode == "lifecycle-failure" {
						for _, call := range s.requests {
							if strings.HasPrefix(call, "DELETE") {
								t.Fatal("failed setup/create was cleaned up")
							}
						}
					} else if len(s.rows["vpc"]) != 0 || len(s.rows["os"]) != 1 {
						t.Fatal("lifecycle teardown ownership failed")
					}
					return
				}
				if mode == "unstable-selected" || mode == "rack" || mode == "dpu" {
					if err != nil || out.Verdict != result.NotReady || !r.j.Unsettled() || labelWrites != 0 {
						t.Fatalf("frozen unstable cohort accepted: %s %v", out.Verdict, err)
					}
					return
				}
				if err != nil || out.Verdict != result.Passed || r.j.Unsettled() {
					t.Fatalf("create: %s %v reasons=%v detail=%s", out.Verdict, err, out.Reasons, out.Detail)
				}
				var detail instanceDetail
				if json.Unmarshal(out.Detail, &detail) != nil || mode != "partial-rerun" && len(detail.BackgroundChanges) == 0 {
					t.Fatal("detail")
				}
				del, dr, dc := instanceCommand(t, s.instanceSite, "instance-delete", "fixture-delete", test.cfg.Journal.Dir, 5)
				del.s = test.s
				del.s.preparing = true
				del.s.Fixture.Teardown = true
				del.s.Instance.OwnedManifest = detail.Manifest
				del.rest.settings = del.s
				del.rest.authClient = auth
				del.rest.client.Transport = test.rest.client.Transport
				if strings.HasPrefix(mode, "cleanup-") {
					owned, e := readOwned(test.cfg.Journal.Dir, detail.Manifest)
					if e != nil {
						t.Fatal(e)
					}
					for _, h := range owned.Cohort {
						absentInstance(s.instanceSite, h)
						delete(s.state[probeapi.FamilyRestMachines][h.RestID].RestMachine.Labels, instanceLabel)
					}
					retained := owned.Cohort[0]
					owned.Cohort = nil
					var ledger fixtureLedger
					if e = fixtureFile(test.cfg.Journal.Dir, owned.Fixture["path"], &ledger, owned.Fixture["sha256"]); e != nil {
						t.Fatal(e)
					}
					if mode == "cleanup-retained-error" {
						ledger.Records = append(ledger.Records, fixtureRecord{Kind: "association:" + retained.RestID, ID: retained.RestID, Path: "instance/type/" + owned.Type + "/machine/" + retained.RestID, Provider: true, Owned: true, Proof: 7, Spec: map[string]any{"hostId": retained.ID, "instanceTypeId": owned.Type}})
					}
					for i := range ledger.Records {
						if ledger.Records[i].Kind == "type" && mode != "cleanup-retained-error" {
							ledger.Records[i].Owned, ledger.Records[i].Proof = true, 7
						}
					}
					hash, e := writeOnce(test.cfg.Journal.Dir, "cleanup.fixture.json", ledger)
					if e != nil {
						t.Fatal(e)
					}
					owned.Fixture = map[string]string{"path": "cleanup.fixture.json", "sha256": hash}
					raw, _ := json.Marshal(owned.Fixture)
					if e = os.WriteFile(filepath.Join(test.cfg.Journal.Dir, "fixture.fixture-owner.json"), raw, 0600); e != nil {
						t.Fatal(e)
					}
					if _, e = writeOnce(test.cfg.Journal.Dir, "cleanup.owned.json", owned); e != nil {
						t.Fatal(e)
					}
					del.s.Instance.OwnedManifest = "cleanup.owned.json"
					for _, u := range s.state[probeapi.FamilyRestMachines] {
						u.RestMachine.InstanceType = nil
					}
					if mode == "cleanup-retained-error" {
						s.state[probeapi.FamilyRestMachines][retained.RestID].RestMachine.InstanceType = new(owned.Type)
						s.machineStatus(retained.RestID, "Error")
					} else {
						s.rows["type"] = nil
					}
					if mode == "cleanup-config-missing" {
						s.allocationMode = "missing"
					}
				}
				var missing CohortHost
				if mode == "missing-candidate" || mode == "all-instances-gone" {
					owned, e := readOwned(test.cfg.Journal.Dir, detail.Manifest)
					if e != nil {
						t.Fatal(e)
					}
					if mode == "all-instances-gone" {
						for _, host := range owned.Cohort {
							absentInstance(s.instanceSite, host)
						}
					} else {
						missing = owned.Cohort[0]
						absentInstance(s.instanceSite, missing)
						del.s.Instance.DeleteCount = 4
					}
				}
				// The existing native fixture reports tenant-org for its created instances.
				for id, u := range s.state[probeapi.FamilyInstance] {
					u.Instance.Tenant = new("tenant")
					s.state[probeapi.FamilyInstance][id] = u
				}
				s.frame = -1
				if mode == "background" {
					del.s.Instance.DeleteCount = 2
					partial, e := runInstance(del, dr, dc, false)
					var remaining instanceDetail
					if e != nil || partial.Verdict != result.Passed || json.Unmarshal(partial.Detail, &remaining) != nil || len(s.rows["vpc"]) != 1 {
						t.Fatalf("partial teardown ran early: %s %v", partial.Verdict, e)
					}
					del, dr, dc = instanceCommand(t, s.instanceSite, "instance-delete", "fixture-delete-rest", test.cfg.Journal.Dir, 5)
					del.s = test.s
					del.s.preparing = true
					del.s.Fixture.Teardown = true
					del.s.Instance.OwnedManifest = remaining.Manifest
					del.rest.settings = del.s
					del.rest.authClient = auth
					del.rest.client.Transport = test.rest.client.Transport
				}
				beforeWrites := writes
				out, err = runInstance(del, dr, dc, false)
				if mode == "cleanup-retained-error" {
					if out.Verdict == result.Passed || writes != beforeWrites || !strings.Contains(strings.Join(out.Reasons, " "), "cleanup association") {
						t.Fatalf("unsafe cleanup dispatched: %s writes=%d err=%v reasons=%v", out.Verdict, writes-beforeWrites, err, out.Reasons)
					}
					return
				}
				if err != nil || out.Verdict != result.Passed || dr.j.Unsettled() {
					t.Fatalf("delete: %s %v reasons=%v detail=%s", out.Verdict, err, out.Reasons, out.Detail)
				}
				if strings.HasPrefix(mode, "cleanup-config-") || mode == "all-instances-gone" {
					var detail instanceDetail
					if json.Unmarshal(out.Detail, &detail) != nil {
						t.Fatal("cleanup detail missing")
					}
					fixture := detail.Evidence["fixture"].(map[string]any)
					if fixture["measurement"].(map[string]any)["verdict"] != "skipped" || len(out.Stages) != 0 {
						t.Fatal("cleanup claimed measurement timings")
					}
				}
				if mode == "missing-candidate" {
					var detail instanceDetail
					if json.Unmarshal(out.Detail, &detail) != nil {
						t.Fatal("remaining manifest missing")
					}
					remaining, e := readOwned(test.cfg.Journal.Dir, detail.Manifest)
					if e != nil || len(remaining.Cohort) != 1 || remaining.Cohort[0].InstanceID != missing.InstanceID || len(s.rows["vpc"]) != 1 {
						t.Fatal("unreconciled ownership or fixture lost")
					}
					return
				}
				if (mode == "fabric-root" || mode == "fallback-root") && (creates["/v2/org/provider-org/nico/allocation"] != 1 || len(s.rows["ipallocation"]) != 0 || mode == "fabric-root" && (len(s.rows["providerBlocks"]) != 1 || creates["/v2/org/provider-org/nico/ipblock"] != 0) || mode == "fallback-root" && (len(s.rows["ipblock"]) != 0 || creates["/v2/org/provider-org/nico/ipblock"] != 1)) {
					t.Fatal("borrowed fabric block mutated or owned allocation not released")
				}
				wantVPC := 0
				if mode == "partial-rerun" {
					wantVPC = 1
				}
				if len(s.rows["vpc"]) != wantVPC || len(s.rows["os"]) != 1 {
					t.Fatal("owned release or borrowed retention failed")
				}
			})
		})
	}
}

func TestFixtureSidecarMetadata(t *testing.T) {
	for _, mode := range []string{"valid", "stale", "future", "epoch", "site", "overflow"} {
		t.Run(mode, func(t *testing.T) {
			f, s, _ := fixtureRig(t)
			s.mode = mode
			settings := f.in.t.s
			settings.preparing = true
			settings.restEpoch = s.process
			_, err := readInstanceSnapshot(f.in.ctx, f.in.t.p, f.end, settings)
			if (err == nil) != (mode == "valid") {
				t.Fatalf("sidecar metadata: %v", err)
			}
		})
	}
}

func TestFixtureTargetedAccount(t *testing.T) {
	for _, mode := range []string{"fresh", "borrowed-disabled", "borrowed-enabled", "missing-readback", "concurrent", "concurrent-scope", "slow-healthy", "slow-pool", "slow-pods", "slow-probe", "slow-always", "renewal-race"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				f, s, _ := fixtureRig(t)
				body := map[string]any{"tenantOrg": "tenant", "infrastructureProviderId": f.provider}
				row := map[string]any{"id": instanceUUID(810), "tenantOrg": "tenant", "tenantId": f.tenant, "infrastructureProviderId": f.provider, "status": "Ready", "targetedInstanceCreation": mode == "borrowed-enabled", "siteCapabilities": []any{map[string]any{"siteIds": []any{}, "targetedInstanceCreation": false}, map[string]any{"siteIds": []any{instanceUUID(899)}, "targetedInstanceCreation": true}}}
				patches := 0
				f.end = time.Now().Add(20 * time.Second)
				f.in.t.rest.client.Transport = labelRetryTransport(func(req *http.Request) (*http.Response, error) {
					if req.Method == http.MethodPost {
						s.rows["account"] = []map[string]any{row}
					} else {
						patches++
						var patch map[string]any
						if json.NewDecoder(req.Body).Decode(&patch) != nil || !containsSpec(map[string]any{"siteCapabilities": []any{map[string]any{"siteIds": []any{}, "targetedInstanceCreation": false}, map[string]any{"siteIds": []any{instanceUUID(899)}, "targetedInstanceCreation": true}, map[string]any{"siteIds": []any{f.in.t.s.Fixture.SiteID}, "targetedInstanceCreation": true}}}, patch) {
							t.Fatal("capability patch changes other sites")
						}
						if mode != "missing-readback" {
							row["targetedInstanceCreation"] = true
						}
					}
					return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"id":"` + instanceUUID(810) + `"}`))}, nil
				})
				if strings.HasPrefix(mode, "borrowed-") {
					s.rows["account"] = []map[string]any{row}
				}
				account, err := f.ensure("account", body, "Ready")
				if err != nil {
					t.Fatal(err)
				}
				if strings.HasPrefix(mode, "concurrent") {
					s.readHook = func(kind string) {
						if kind == "account" {
							s.readHook = nil
							row["siteCapabilities"] = append(list(row["siteCapabilities"]), map[string]any{"siteIds": []any{instanceUUID(898)}, "targetedInstanceCreation": true})
							if mode == "concurrent-scope" {
								row["infrastructureProviderId"] = instanceUUID(999)
							}
						}
					}
				}
				if mode == "renewal-race" {
					rest := f.in.t.rest
					rest.settings.Rest.Auth = fixtureAuth{TokenURL: "https://identity.example/token", CredentialsDir: t.TempDir()}
					if err := writeIdentity(rest.settings.Rest.Auth.CredentialsDir, "provider", clientIdentity{"client", "secret"}); err != nil {
						t.Fatal(err)
					}
					rest.authClient = &http.Client{Transport: labelRetryTransport(func(*http.Request) (*http.Response, error) {
						row["siteCapabilities"] = append(list(row["siteCapabilities"]), map[string]any{"siteIds": []any{instanceUUID(898)}, "targetedInstanceCreation": true})
						return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"access_token":"token","expires_in":300}`))}, nil
					})}
				}
				if strings.HasPrefix(mode, "slow-") {
					f.end = time.Now().Add(time.Minute)
					s.readHook = func(kind string) {
						if kind != "account" {
							return
						}
						if mode != "slow-always" {
							s.readHook = nil
						}
						time.Sleep(20 * time.Second)
						switch mode {
						case "slow-pool":
							f.in.store.(*instanceMetrics).unhealthy.Store(true)
						case "slow-pods":
							f.in.t.deps.Pods.(*fakePods).pods["core"][0].Restarts++
						case "slow-probe":
							s.process++
						}
					}
				}
				err = f.targetedAccount(account)
				if (err == nil) != (mode == "fresh" || mode == "borrowed-enabled" || mode == "slow-healthy") || patches != map[string]int{"fresh": 1, "slow-healthy": 1, "borrowed-disabled": 0, "borrowed-enabled": 0, "missing-readback": 1}[mode] {
					t.Fatalf("capability mode=%s patches=%d err=%v", mode, patches, err)
				}
			})
		})
	}
}

type identityBodyError struct{ error }

func (e identityBodyError) Read([]byte) (int, error) { return 0, e.error }
func (e identityBodyError) Close() error             { return nil }
func TestFixtureIdentityCauses(t *testing.T) {
	for _, mode := range []string{"deadline", "cancel", "body"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				f, _, _ := fixtureRig(t)
				ctx, cancel := context.WithCancel(f.in.ctx)
				defer cancel()
				f.in.t.rest.authClient = &http.Client{Transport: labelRetryTransport(func(req *http.Request) (*http.Response, error) {
					if mode == "body" {
						return &http.Response{StatusCode: 200, Body: identityBodyError{fmt.Errorf("credential-secret: %w", context.DeadlineExceeded)}}, nil
					}
					if mode == "cancel" {
						cancel()
					}
					<-req.Context().Done()
					return nil, fmt.Errorf("credential-secret: %w", req.Context().Err())
				})}
				_, err := f.in.t.rest.grant(ctx, "https://identity.example/secret", url.Values{})
				cause := context.DeadlineExceeded
				if mode == "cancel" {
					cause = context.Canceled
				}
				if !errors.Is(err, cause) || instanceExecutionError(err) != nil || strings.Contains(err.Error(), "credential-secret") || strings.Contains(err.Error(), "identity.example") || mode != "cancel" && ctx.Err() != nil {
					t.Fatalf("lost or unsafe cause: %v", err)
				}
			})
		})
	}
}

func TestFixtureAllocationDispatchGate(t *testing.T) {
	for _, mode := range []string{"healthy", "failed", "missing", "unit-epoch", "timeout", "setup", "closed"} {
		t.Run(mode, func(t *testing.T) {
			f, s, _ := fixtureRig(t)
			in := f.in
			s.allocationMode = mode
			if mode == "setup" || mode == "closed" {
				s.allocationMode = "failed"
			}
			in.fixturePhase = mode == "setup"
			in.fixtureRESTClosed = mode == "closed"
			in.t.s.restEpoch = s.process
			if mode == "unit-epoch" {
				// The run froze the sidecar's previous process before restart.
				in.t.s.restEpoch--
			}
			in.health.Store(nil)
			in.store = &instanceMetrics{}
			in.cp = capture(in.ctx, in.t.deps.Pods, in.t.cfg.ControlPlane, true).Pods
			in.mat = capture(in.ctx, in.t.deps.Pods, []config.PodSelector{{Namespace: in.t.cfg.Namespace, LabelSelector: in.t.s.Mat.Selector}}).Pods
			err := in.safety()
			health := in.health.Load()
			if health == nil || (health.err != nil) != (err != nil) {
				t.Fatal("health result was not published")
			}
			if err != nil && !errors.Is(in.safety(), err) {
				t.Fatal("cached health failure lost its typed cause")
			}
			ok := mode == "healthy" || mode == "setup" || mode == "closed"
			if (err == nil) != ok || (s.configReads == 0) != (mode == "setup" || mode == "closed") {
				t.Fatalf("allocation gate reads=%d err=%v", s.configReads, err)
			}
			if mode == "unit-epoch" && !strings.Contains(err.Error(), "source epoch") {
				t.Fatalf("restart did not reach the unit epoch check: %v", err)
			}
		})
	}
}
func TestFixtureProviderCauses(t *testing.T) {
	for _, cause := range []error{context.Canceled, context.DeadlineExceeded, journal.ErrIO} {
		t.Run(cause.Error(), func(t *testing.T) {
			f, s, r := fixtureRig(t)
			f.in.t.s.Rest.Auth = fixtureAuth{ProviderDirectory: fixtureCredentials(t, "provider"), CredentialsDir: t.TempDir()}
			s.rows["provider"] = []map[string]any{{"id": f.provider}}
			if errors.Is(cause, context.Canceled) || errors.Is(cause, context.DeadlineExceeded) {
				s.providerError = cause
			} else {
				r.j.Inject(func(*os.File, []byte) error { return cause })
			}
			_, err := f.in.beginFixture(s.inv)
			if !errors.Is(err, cause) || (instanceExecutionError(err) != nil) != errors.Is(cause, journal.ErrIO) {
				t.Fatalf("lost provider cause: %v", err)
			}
		})
	}
}
func fixtureCredentials(t *testing.T, id string) string {
	t.Helper()
	dir := t.TempDir()
	for key, value := range map[string]string{"client-id": id, "client-secret": "secret"} {
		if err := os.WriteFile(filepath.Join(dir, key), []byte(value), 0600); err != nil {
			t.Fatal(err)
		}
	}
	return dir
}

func frozenFixture(t *testing.T) (*fixtureRun, *fixtureSite) {
	t.Helper()
	f, site, _ := fixtureRig(t)
	f.in.t.s.preparing, f.in.fixturePhase, f.in.fixtureRESTClosed = true, true, false
	f.kind = instanceUUID(900)
	f.epoch, f.in.t.s.restEpoch = site.process, site.process
	for _, u := range site.state[probeapi.FamilyRestMachines] {
		u.RestMachine.MachineCapabilities = f.in.t.s.Fixture.MachineCapabilities
	}
	snapshot, err := readInstanceSnapshot(f.in.ctx, f.in.t.p, f.end, f.in.t.s)
	if err != nil {
		t.Fatal(err)
	}
	f.in.mat = map[string]podState{"mat/mat-0": {UID: "uid-1"}}
	f.in.plan, err = planInstances(snapshot, site.inv, f.in.mat, f.in.t.s, f.in.t.cfg.RunID, f.in.t.cfg.Namespace)
	if err != nil {
		t.Fatal(err)
	}
	f.in.latest = snapshot
	for _, h := range f.in.plan.Cohort {
		f.ledger.Cohort[h.ID] = h.RestID
	}
	return f, site
}
func TestFixtureCurrentWriteEvidence(t *testing.T) {
	for _, mode := range []string{"valid", "rest-error", "foreign-type", "foreign-label", "expected-unlinked", "association", "quota-race", "teardown-orphan", "teardown-foreign-type", "teardown-replacement", "teardown-late-replacement", "teardown-resume", "cleanup-valid", "cleanup-earlier-run", "cleanup-rest-error", "cleanup-dpu-error", "cleanup-foreign-label", "cleanup-rest-id"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				f, site := frozenFixture(t)
				h := f.in.plan.Cohort[0]
				switch mode {
				case "rest-error":
					site.machineStatus(h.RestID, "Error")
				case "foreign-type", "association":
					site.state[probeapi.FamilyRestMachines][h.RestID].RestMachine.InstanceType = new(instanceUUID(999))
				case "foreign-label":
					site.state[probeapi.FamilyRestMachines][h.RestID].RestMachine.Labels[instanceLabel] = "foreign-run-b0"
				case "expected-unlinked":
					site.state[probeapi.FamilyExpected][h.ExpectedRecord].Expected.Linked = false
				}
				time.Sleep(30 * time.Second)
				f.in.health.Store(&instanceHealth{at: time.Now()})
				var err error
				if strings.HasPrefix(mode, "cleanup-") {
					if mode == "cleanup-earlier-run" {
						delete(f.ledger.Cohort, h.ID)
					}
					f.in.plan.Cohort = nil
					f.ledger.Records = []fixtureRecord{{Kind: "type", ID: f.kind}, {Kind: "association:" + h.RestID, ID: h.RestID, Path: "instance/type/" + f.kind + "/machine/" + h.RestID, Provider: true, Owned: true, Proof: 7, Spec: map[string]any{"hostId": h.ID, "instanceTypeId": f.kind}}}
					site.state[probeapi.FamilyRestMachines][h.RestID].RestMachine.InstanceType = new(f.kind)
					changed := false
					site.readHook = func(kind string) {
						if kind != "status" || changed {
							return
						}
						changed = true
						switch mode {
						case "cleanup-rest-error":
							site.machineStatus(h.RestID, "Error")
						case "cleanup-dpu-error":
							u := site.state[probeapi.FamilyDpu][h.DPUs[0]]
							u.Stage = probeapi.StageError
							site.state[probeapi.FamilyDpu][u.ID] = u
						case "cleanup-foreign-label":
							site.state[probeapi.FamilyRestMachines][h.RestID].RestMachine.Labels[instanceLabel] = "foreign-run-b0"
						case "cleanup-rest-id":
							other := instanceUUID(2)
							u, v := site.state[probeapi.FamilyRestMachines][h.RestID], site.state[probeapi.FamilyRestMachines][other]
							u.ID, v.ID = other, h.RestID
							site.state[probeapi.FamilyRestMachines][u.ID], site.state[probeapi.FamilyRestMachines][v.ID] = u, v
						}
					}
					site.mode = "first-401"
					err = f.teardown()
				} else if mode == "quota-race" {
					constraint := map[string]any{"id": instanceUUID(811), "resourceType": "InstanceType", "resourceTypeId": f.kind, "constraintType": "Reserved", "constraintValue": 100}
					row := map[string]any{"id": instanceUUID(810), "name": "fixture-compute", "siteId": f.in.t.s.Fixture.SiteID, "tenantId": f.tenant, "status": "Registered", "allocationConstraints": []any{constraint}}
					site.rows["compute"] = []map[string]any{row}
					f.previous["compute"] = fixtureRecord{ID: text(row, "id"), Owned: true, Proof: 7}
					site.readHook = func(kind string) {
						if kind == "status" {
							constraint["constraintValue"] = 200
						}
					}
					body := normalized(row)
					list(body["allocationConstraints"])[0].(map[string]any)["constraintValue"] = 150
					_, err = f.ensure("compute", body, "Registered")
				} else if mode == "association" {
					f.in.latest.rest[h.ID].RestMachine.InstanceType = nil
					err = f.associate()
				} else if strings.HasPrefix(mode, "teardown-") {
					row := map[string]any{"id": instanceUUID(810), "siteId": f.in.t.s.Fixture.SiteID, "tenantId": f.tenant, "name": "fixture-vpc", "status": "Ready"}
					site.rows["vpc"] = []map[string]any{row}
					f.ledger.Records = []fixtureRecord{{Kind: "vpc", ID: text(row, "id"), Path: "vpc/" + text(row, "id"), Owned: true, Proof: 7, Spec: normalized(map[string]any{"name": "fixture-vpc", "siteId": f.in.t.s.Fixture.SiteID})}}
					f.in.t.s.Fixture.SetupSeconds = 120
					if mode == "teardown-foreign-type" {
						f.ledger.Records = append([]fixtureRecord{{Kind: "type", ID: f.kind, Owned: true, Proof: 7}}, f.ledger.Records...)
					}
					if mode == "teardown-replacement" {
						f.ledger.Records = append([]fixtureRecord{{Kind: "os", ID: instanceUUID(820), Owned: true, Proof: 7, Spec: map[string]any{"name": "fixture-os"}}}, f.ledger.Records...)
						site.rows["os"] = []map[string]any{{"id": instanceUUID(821), "name": "fixture-os"}}
					}
					if mode == "teardown-resume" {
						site.rows["vpc"] = nil
						f.in.t.s.Rest.Auth = fixtureAuth{TenantDirectory: fixtureCredentials(t, "tenant-client"), TenantClientID: "tenant-client", CredentialsDir: t.TempDir()}
						err = f.reuse()
						if err != nil {
							t.Fatal(err)
						}
						f.in.evidence, err = openEvidence(t.TempDir(), "resume")
						if err != nil {
							t.Fatal(err)
						}
						t.Cleanup(func() { _ = f.in.evidence.finish() })
					}
					reads := 0
					site.readHook = func(kind string) {
						if kind == "vpc" {
							reads++
						}
						if kind == "status" && mode == "teardown-late-replacement" && reads >= 2 {
							row["id"] = instanceUUID(811)
						}
						if kind == "vpc" && mode == "teardown-orphan" {
							site.state[probeapi.FamilyRestInstances][instanceUUID(980)] = probeapi.Unit{ID: instanceUUID(980), Type: probeapi.UnitTypeInstance, Source: probeapi.SourceRest, Stage: probeapi.StageRegistered, State: "Pending", Instance: &probeapi.InstanceFields{Name: new("external-pending"), Tenant: new(f.tenant)}}
							time.Sleep(30 * time.Second)
							f.in.health.Store(&instanceHealth{at: time.Now()})
						}
					}
					err = f.teardown()
				} else {
					_, err = f.mutate(http.MethodPost, "vpc", map[string]any{"name": "fixture-vpc"}, false, -1, nil)
				}
				expected := 0
				if mode == "valid" || mode == "cleanup-valid" || mode == "cleanup-earlier-run" {
					expected = 1
				}
				if (err == nil) != (mode == "valid" || mode == "teardown-resume") || site.writes != expected {
					t.Fatalf("unsafe fixture %s: writes=%d err=%v", mode, site.writes, err)
				}
			})
		})
	}
}

type slowFixtureNative struct{ *instanceSite }

func (p slowFixtureNative) Config(ctx context.Context, q probe.ConfigFilter) (probeapi.ConfigResponse, error) {
	if deadline, ok := ctx.Deadline(); !ok || time.Until(deadline) > 15*time.Second {
		time.Sleep(18 * time.Second)
	}
	return p.instanceSite.Config(ctx, q)
}
func TestFixtureAuthorizationAfterPreparation(t *testing.T) {
	for _, mode := range []string{"rest", "measurement"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				f, site := frozenFixture(t)
				rest := f.in.t.rest
				rest.settings.Rest.Auth = fixtureAuth{TokenURL: "https://identity.example/token", CredentialsDir: t.TempDir()}
				for _, name := range []string{"tenant", "provider"} {
					if err := writeIdentity(rest.settings.Rest.Auth.CredentialsDir, name, clientIdentity{"client", "secret"}); err != nil {
						t.Fatal(err)
					}
				}
				expiry := time.Now().Add(35 * time.Second)
				if mode == "measurement" {
					expiry = time.Now().Add(51 * time.Second)
				}
				rest.auth.tokens = map[bool]oauthToken{false: {"old-token", expiry}, true: {"old-token", expiry}}
				grants, writes := 0, 0
				transport := labelRetryTransport(func(req *http.Request) (*http.Response, error) {
					if req.URL.Path == "/token" {
						grants++
						return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"access_token":"new-token","expires_in":300}`))}, nil
					}
					writes++
					if req.Header.Get("Authorization") != "Bearer new-token" {
						t.Error("expired cached token dispatched")
					}
					return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{"id":"` + instanceUUID(810) + `"}`))}, nil
				})
				rest.authClient, rest.client = &http.Client{Transport: transport}, &http.Client{Transport: transport}
				f.in.t.p = slowFixtureNative{site.instanceSite}
				site.readHook = func(kind string) {
					if kind == "status" {
						time.Sleep(18 * time.Second)
					}
				}
				var err error
				if mode == "measurement" {
					var err error
					f.in.evidence, err = openEvidence(t.TempDir(), "auth-measurement")
					if err != nil {
						t.Fatal(err)
					}
					t.Cleanup(func() { _ = f.in.evidence.finish() })
					f.in.previous = maps.Clone(f.in.plan.Baseline)
					f.in.t.s.preparing = false
					time.Sleep(15 * time.Second)
					f.in.health.Store(&instanceHealth{at: time.Now()})
				}
				switch mode {
				case "measurement":
					f.in.positions = map[string]int{}
					h := f.in.plan.Cohort[0]
					f.in.positions[h.ID] = 0
					labels := maps.Clone(f.in.latest.rest[h.ID].RestMachine.Labels)
					labels[instanceLabel] = f.in.selectorValue(h.Burst)
					_, err = f.in.send("labels", http.MethodPatch, "machine/"+h.RestID, map[string]any{"labels": labels}, 0, h.ID)
				default:
					_, err = f.mutate(http.MethodPost, "vpc", map[string]any{"name": "fixture-vpc"}, false, -1, nil)
				}
				if err != nil || grants != 1 || writes != 1 {
					t.Fatalf("post-preparation auth: grants=%d writes=%d err=%v", grants, writes, err)
				}
			})
		})
	}
}

func TestFixtureComputeGrowth(t *testing.T) {
	for _, value := range []any{10, -1, "1e3"} {
		t.Run(fmt.Sprint(value), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				f, site, _ := fixtureRig(t)
				old := map[string]any{"id": instanceUUID(820), "resourceType": "InstanceType", "resourceTypeId": f.kind, "constraintType": "OnDemand", "constraintValue": value}
				desired := maps.Clone(old)
				delete(desired, "id")
				desired["constraintValue"] = 20
				body := map[string]any{"name": "fixture-compute", "tenantId": f.tenant, "siteId": f.in.t.s.Fixture.SiteID, "allocationConstraints": []any{desired}}
				row := maps.Clone(body)
				row["id"] = instanceUUID(810)
				row["status"] = "Registered"
				row["allocationConstraints"] = []any{old}
				site.rows["compute"] = []map[string]any{row}
				f.previous["compute"] = fixtureRecord{ID: text(row, "id"), Owned: true, Proof: 7}
				writes := 0
				f.in.t.rest.client.Transport = labelRetryTransport(func(*http.Request) (*http.Response, error) {
					writes++
					old["constraintValue"] = 20
					return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(`{}`))}, nil
				})
				_, err := f.ensure("compute", body, "Registered")
				if (err == nil) != (value == 10) || writes != map[bool]int{true: 1, false: 0}[value == 10] {
					t.Fatalf("unknown allocation grew: value=%v writes=%d err=%v", value, writes, err)
				}
			})
		})
	}
}

func TestFixtureLegacyLedgerRefusesCheckpoint(t *testing.T) {
	for field, value := range map[string]any{"supported": nil, "keycloak": []any{map[string]any{"kind": "identity-client", "id": "owned-client"}}, "identity": map[string]string{"tenant": "owned-client"}, "pendingIdentity": true} {
		t.Run(field, func(t *testing.T) {
			f, _, _ := fixtureRig(t)
			raw, err := json.Marshal(f.ledger)
			if err != nil {
				t.Fatal(err)
			}
			var ledger map[string]any
			if err := json.Unmarshal(raw, &ledger); err != nil {
				t.Fatal(err)
			}
			if field != "supported" {
				ledger[field] = value
			}
			dir := f.in.t.cfg.Journal.Dir
			hash, err := writeOnce(dir, "legacy.fixture.json", ledger)
			if err != nil {
				t.Fatal(err)
			}
			index, err := json.Marshal(map[string]string{"path": "legacy.fixture.json", "sha256": hash})
			if err != nil {
				t.Fatal(err)
			}
			path := filepath.Join(dir, f.in.t.s.Fixture.Name+".fixture-owner.json")
			if err := os.WriteFile(path, index, 0600); err != nil {
				t.Fatal(err)
			}
			err = f.load()
			if field == "supported" {
				if err != nil {
					t.Fatal(err)
				}
			} else if err == nil || !strings.Contains(err.Error(), "operator reconciliation") {
				t.Fatalf("legacy %s admitted: %v", field, err)
			}
			if err := f.checkpoint(); err != nil {
				t.Fatal(err)
			}
			after, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			if (string(index) == string(after)) != (field != "supported") {
				t.Fatal("checkpoint lost legacy obligations or refused a supported ledger")
			}
		})
	}
}

func TestFixtureWriteHealth(t *testing.T) {
	for _, phase := range []string{"teardown", "cleanup", "labels", "dispatch", "dispatch-after"} {
		for _, unhealthy := range []bool{false, true} {
			t.Run(fmt.Sprintf("%s/%v", phase, unhealthy), func(t *testing.T) {
				synctest.Test(t, func(t *testing.T) {
					f, site := frozenFixture(t)
					metrics := f.in.store.(*instanceMetrics)
					var err error
					if phase == "labels" {
						f.in.t.s.preparing, f.in.fixturePhase = false, false
						f.in.evidence, err = openEvidence(t.TempDir(), "health-labels")
						if err != nil {
							t.Fatal(err)
						}
						t.Cleanup(func() { _ = f.in.evidence.finish() })
						f.in.previous, f.in.classes = maps.Clone(f.in.plan.Baseline), map[string]string{}
						f.in.t.rest.pace = rate.NewLimiter(rate.Every(20*time.Second), 1)
						if !f.in.t.rest.pace.Allow() {
							t.Fatal("missing initial pacing token")
						}
						go func() { time.Sleep(10 * time.Second); metrics.unhealthy.Store(unhealthy) }()
						h := f.in.plan.Cohort[0]
						f.in.positions = map[string]int{h.ID: 0}
						labels := maps.Clone(f.in.latest.rest[h.ID].RestMachine.Labels)
						labels[instanceLabel] = f.in.selectorValue(h.Burst)
						_, err = f.in.send("labels", http.MethodPatch, "machine/"+h.RestID, map[string]any{"labels": labels}, 0, h.ID)
					} else {
						row := map[string]any{"id": instanceUUID(810), "siteId": f.in.t.s.Fixture.SiteID, "tenantId": f.tenant, "name": "fixture-vpc"}
						site.rows["vpc"] = []map[string]any{row}
						if phase == "cleanup" {
							f.in.plan.Cohort = nil
							f.cleanupHosts = map[string]bool{}
						}
						if strings.HasPrefix(phase, "dispatch") {
							r := newRig(t)
							checks := 0
							f.in.w = writer.New(r.j, labelRetryGate(func() error {
								if unhealthy && r.j.Counts().Attempts > 0 {
									checks++
									if checks == 1 && phase == "dispatch" || checks == 2 && phase == "dispatch-after" {
										time.Sleep(20 * time.Second)
									}
								}
								return r.gate.Check()
							}), site.control.Stopper(), f.end)
						} else {
							site.readHook = func(kind string) {
								if kind == "vpc" {
									site.readHook = nil
									time.Sleep(20 * time.Second)
									metrics.unhealthy.Store(unhealthy)
								}
							}
						}
						_, err = f.mutate(http.MethodDelete, "vpc/"+text(row, "id"), nil, false, -1, row, f.unchanged("vpc", row))
					}
					if strings.HasPrefix(phase, "dispatch") && unhealthy && !errors.Is(err, errSafetyStop) {
						t.Fatalf("lost typed health stop: %v", err)
					}
					want := 1
					if unhealthy {
						want = 0
					}
					if site.writes != want || (err != nil) != unhealthy {
						t.Fatalf("%s unhealthy=%v writes=%d err=%v", phase, unhealthy, site.writes, err)
					}
				})
			})
		}
	}
}

func TestFixtureFinalHealthContention(t *testing.T) {
	for _, mode := range []string{"healthy", "lease-loss", "checking-fresh", "stale", "unhealthy", "never"} {
		t.Run(mode, func(t *testing.T) {
			f, site := frozenFixture(t)
			r := newRig(t)
			checks := 0
			var done, release chan struct{}
			waited := false
			f.in.w = writer.New(r.j, labelRetryGate(func() error {
				if r.j.Counts().Attempts > 0 {
					checks++
				}
				if checks == 2 && done == nil {
					switch mode {
					case "lease-loss", "checking-fresh":
						if mode == "lease-loss" {
							time.Sleep(16 * time.Second)
						}
						f.in.safetyMu.Lock()
						done = make(chan struct{})
						if mode == "checking-fresh" {
							release = make(chan struct{})
						}
						go func() {
							if mode == "lease-loss" {
								time.Sleep(time.Second)
								r.gate.lost.Store(true)
								time.Sleep(4 * time.Second)
							} else {
								select {
								case <-release:
								case <-time.After(5 * time.Second):
									waited = true
								}
							}
							f.in.health.Store(&instanceHealth{at: time.Now()})
							f.in.safetyMu.Unlock()
							close(done)
						}()
					case "stale":
						f.in.health.Store(&instanceHealth{at: time.Now().Add(-16 * time.Second)})
					case "unhealthy":
						f.in.health.Store(&instanceHealth{at: time.Now(), err: instanceIssue("API pool saturated")})
					case "never":
						f.in.health.Store(nil)
					}
				}
				return r.gate.Check()
			}), site.control.Stopper(), f.end)
			row := map[string]any{"id": instanceUUID(810), "siteId": f.in.t.s.Fixture.SiteID, "tenantId": f.tenant, "name": "fixture-vpc"}
			site.rows["vpc"] = []map[string]any{row}
			guard := func() error {
				if err := f.unchanged("vpc", row)(); err != nil {
					return err
				}
				// Model fresh sweeps so the real-clock test isolates mutex contention.
				for family, meta := range f.in.latest.meta {
					meta.SweepStart, meta.ReadAt = jsonx.Time{Time: time.Now().Add(-time.Millisecond)}, jsonx.Time{Time: time.Now()}
					f.in.latest.meta[family] = meta
				}
				if f.in.latest.config.Rest != nil {
					f.in.latest.config.Rest.ReadStart.Time = time.Now().Add(-time.Millisecond)
				}
				if f.in.latest.config.RestSite != nil {
					f.in.latest.config.RestSite.ReadStart.Time = time.Now().Add(-time.Millisecond)
				}
				return nil
			}
			_, err := f.mutate(http.MethodDelete, "vpc/"+text(row, "id"), nil, false, -1, row, guard)
			if release != nil {
				close(release)
			}
			if done != nil {
				<-done
			}
			healthy := mode == "healthy" || mode == "checking-fresh"
			want := 0
			if healthy {
				want = 1
			}
			if waited || site.writes != want || healthy && err != nil || !healthy && !errors.Is(err, errSafetyStop) || mode == "lease-loss" && !r.gate.lost.Load() {
				t.Fatalf("mode=%s waited=%v writes=%d err=%v", mode, waited, site.writes, err)
			}
		})
	}
}
