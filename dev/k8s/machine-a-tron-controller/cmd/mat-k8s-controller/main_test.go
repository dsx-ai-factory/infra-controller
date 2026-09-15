// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"sync/atomic"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/controller"
	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/sourcelist"
)

func noEnv(string) (string, bool) { return "", false }

// envFrom is an os.LookupEnv over the given variables.
func envFrom(env map[string]string) func(string) (string, bool) {
	return func(key string) (string, bool) {
		v, ok := env[key]
		return v, ok
	}
}

func TestParseOptions_DefaultsAndEnvironment(t *testing.T) {
	opts, err := parseOptions(nil, noEnv)
	require.NoError(t, err)
	assert.Equal(t, "nico-system", opts.namespace)
	assert.Equal(t, 30*time.Second, opts.syncInterval)
	assert.Equal(t, sourcelist.DefaultAddr, opts.sourceListAddr)
	assert.Equal(t, sourcelist.DefaultDebounce, opts.sourceListDebounce)
	assert.Equal(t, defaultHealthAddr, opts.healthAddr)
	assert.Equal(t, defaultHealthStaleAfter, opts.healthStaleAfter)

	env := map[string]string{
		"NAMESPACE":            "other",
		"SOURCE_LIST_ADDR":     "127.0.0.1:9090",
		"SOURCE_LIST_DEBOUNCE": "7s",
		"HEALTH_ADDR":          ":9091",
		"HEALTH_STALE_AFTER":   "3m",
	}
	opts, err = parseOptions([]string{"--sync-interval=10s"}, envFrom(env))
	require.NoError(t, err)
	assert.Equal(t, "other", opts.namespace)
	assert.Equal(t, 10*time.Second, opts.syncInterval, "flags override the environment")
	assert.Equal(t, "127.0.0.1:9090", opts.sourceListAddr)
	assert.Equal(t, 7*time.Second, opts.sourceListDebounce)
	assert.Equal(t, ":9091", opts.healthAddr)
	assert.Equal(t, 3*time.Minute, opts.healthStaleAfter)

	// A variable set to the empty value is the flag's default like any other
	// value: it disables the listener instead of restoring the built-in
	// address.
	opts, err = parseOptions(nil, envFrom(map[string]string{"SOURCE_LIST_ADDR": "", "HEALTH_ADDR": ""}))
	require.NoError(t, err)
	assert.Empty(t, opts.sourceListAddr)
	assert.Empty(t, opts.healthAddr)
}

func TestParseOptions_RejectsInvalidValues(t *testing.T) {
	const notLoopback = "source-list-addr must be a loopback IP address"
	for _, tc := range []struct {
		name  string
		args  []string
		env   map[string]string
		want  string
		usage bool // the flag package rejected the command line and reported it itself
	}{
		{"zero sync interval", []string{"--sync-interval=0"}, nil, "sync-interval must be positive", false},
		{"negative sync interval", []string{"--sync-interval=-1s"}, nil, "sync-interval must be positive", false},
		{"negative debounce", []string{"--source-list-debounce=-1s"}, nil, "source-list-debounce must not be negative", false},
		{"negative health bound", []string{"--health-stale-after=-1s"}, nil, "health-stale-after must not be negative", false},
		{"unknown flag", []string{"--source-list-address=127.0.0.1:1"}, nil, "flag provided but not defined", true},
		{"source list on every interface", []string{"--source-list-addr=:8090"}, nil, notLoopback, false},
		{"source list on the pod network", []string{"--source-list-addr=10.0.0.5:8090"}, nil, notLoopback, false},
		{"source list on a host name", []string{"--source-list-addr=localhost:8090"}, nil, notLoopback, false},
		{"source list address from the environment", nil, map[string]string{"SOURCE_LIST_ADDR": "0.0.0.0:8090"}, notLoopback, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, err := parseOptions(tc.args, envFrom(tc.env))
			require.Error(t, err)
			assert.Contains(t, err.Error(), tc.want)
			assert.Equal(t, tc.usage, errors.Is(err, errUsage))
		})
	}

	// A zero debounce and a zero health bound are valid (both disable a
	// check), and so is the IPv6 loopback address.
	opts, err := parseOptions([]string{"--source-list-debounce=0", "--health-stale-after=0", "--source-list-addr=[::1]:8090"}, noEnv)
	require.NoError(t, err)
	assert.Zero(t, opts.sourceListDebounce)
	assert.Zero(t, opts.healthStaleAfter)
	assert.Equal(t, "[::1]:8090", opts.sourceListAddr)
}

// stubDiscovery returns a fixed fleet.
type stubDiscovery struct {
	instances []controller.DiscoveredInstance
}

func (s *stubDiscovery) Discover(context.Context) ([]controller.DiscoveredInstance, error) {
	return s.instances, nil
}

// discoveringReconciler does what the real reconciler does first: run
// discovery, which is what feeds the source list registry.
type discoveringReconciler struct {
	discovery controller.Discovery
	passes    atomic.Int64
}

func (r *discoveringReconciler) Reconcile(ctx context.Context) controller.ReconcileResult {
	r.passes.Add(1)
	if _, err := r.discovery.Discover(ctx); err != nil {
		return controller.ReconcileResult{Errors: []error{err}}
	}
	return controller.ReconcileResult{}
}

func freeLoopbackAddr(t *testing.T) string {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	addr := ln.Addr().String()
	require.NoError(t, ln.Close())
	return addr
}

func getBody(t *testing.T, url string) (int, string) {
	t.Helper()
	resp, err := http.Get(url)
	if err != nil {
		return 0, ""
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	require.NoError(t, err)
	return resp.StatusCode, string(body)
}

func TestRun_PublishesDiscoveredSourcesAndServesLiveness(t *testing.T) {
	fleet := []controller.DiscoveredInstance{
		{URL: "https://mat-b.ns.svc.cluster.local:8443", PodName: "mat-b-pod", ServiceName: "mat-b"},
		{URL: "https://mat-a.ns.svc.cluster.local:8443", PodName: "mat-a-pod", ServiceName: "mat-a"},
	}
	opts, err := parseOptions([]string{
		"--sync-interval=20ms",
		"--source-list-debounce=0",
		"--source-list-addr=" + freeLoopbackAddr(t),
		"--health-addr=" + freeLoopbackAddr(t),
		"--health-stale-after=10s",
	}, noEnv)
	require.NoError(t, err)

	drive := &discoveringReconciler{}
	newReconciler := func(d controller.Discovery) reconciler {
		drive.discovery = d
		return drive
	}

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		done <- run(ctx, opts, &stubDiscovery{instances: fleet}, newReconciler, zerolog.Nop())
	}()

	// The source list becomes ready with the discovered fleet, sorted by name.
	var body string
	require.Eventually(t, func() bool {
		var code int
		code, body = getBody(t, "http://"+opts.sourceListAddr+sourcelist.PathSources)
		if code != http.StatusOK {
			return false
		}
		var resp sourcelist.Response
		return json.Unmarshal([]byte(body), &resp) == nil && resp.Ready
	}, 5*time.Second, 10*time.Millisecond, "source list must become ready")
	var resp sourcelist.Response
	require.NoError(t, json.Unmarshal([]byte(body), &resp))
	assert.Equal(t, uint64(1), resp.Generation)
	assert.Equal(t, []sourcelist.Source{
		{Name: "mat-a", BaseURL: "https://mat-a.ns.svc.cluster.local:8443", Pod: "mat-a-pod"},
		{Name: "mat-b", BaseURL: "https://mat-b.ns.svc.cluster.local:8443", Pod: "mat-b-pod"},
	}, resp.Sources)

	// The loop keeps reconciling and the liveness endpoint answers on the
	// pod-network address.
	require.Eventually(t, func() bool { return drive.passes.Load() >= 3 }, 5*time.Second, 10*time.Millisecond)
	code, health := getBody(t, "http://"+opts.healthAddr+"/healthz")
	assert.Equal(t, http.StatusOK, code)
	assert.Equal(t, "ok\n", health)

	cancel()
	select {
	case err := <-done:
		assert.NoError(t, err)
	case <-time.After(5 * time.Second):
		t.Fatal("run did not return after cancellation")
	}
	code, _ = getBody(t, "http://"+opts.healthAddr+"/healthz")
	assert.Zero(t, code, "the liveness listener must be closed after shutdown")
}

func TestRun_FailsWhenAnEndpointCannotListen(t *testing.T) {
	occupied, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	defer occupied.Close()

	opts, err := parseOptions([]string{
		"--sync-interval=1h",
		"--source-list-addr=" + occupied.Addr().String(),
		"--health-addr=",
	}, noEnv)
	require.NoError(t, err)

	drive := &discoveringReconciler{}
	newReconciler := func(d controller.Discovery) reconciler {
		drive.discovery = d
		return drive
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	err = run(ctx, opts, &stubDiscovery{}, newReconciler, zerolog.Nop())
	require.Error(t, err, "binding an occupied port must end the run")
	assert.Contains(t, err.Error(), "source list endpoint")
	assert.NoError(t, ctx.Err(), "run must return on the listen failure, not on the timeout")
}
