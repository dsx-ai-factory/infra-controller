// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package fleetup is the fleet-up command: a run that writes nothing and finds whether the site is ready for a scale
// test, go or notReady, with every readiness check in its result.
package fleetup

import (
	"bytes"
	"cmp"
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"time"

	"gopkg.in/yaml.v3"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/bmc"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/forge"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/history"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/lock"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/readiness"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/run"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/store"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

// Settings are the fleet-up command's own settings. ExpectedRelease "" is notReady. The store is read only when
// CheckStore is set; else the run is diagnostic and notReady.
type Settings struct {
	ExpectedRelease string `yaml:"expectedRelease"`
	Mat             struct {
		Selector    string `yaml:"selector"`
		ControlPort uint16 `yaml:"controlPort"`
	} `yaml:"mat"`
	CheckStore bool                     `yaml:"checkStore"`
	Expected   readiness.ExpectedCounts `yaml:"expected"`
	Bmc        bmc.Settings             `yaml:"bmc"`
	NicoAPI    struct {
		CertDir    string               `yaml:"certDir"`
		Spiffe     forge.SpiffeSettings `yaml:"spiffe"`
		ServerName string               `yaml:"serverName"`
	} `yaml:"nicoApi"`
	Store store.Config `yaml:"store"`
}

// ParseSettings reads the config's settings mapping with the defaults filled in.
func ParseSettings(n yaml.Node) (Settings, error) {
	s := DefaultSettings()
	if err := DecodeSettings(n, &s); err != nil {
		return Settings{}, err
	}
	return s, nil
}

// DefaultSettings are fleet-up's settings when the config sets none.
func DefaultSettings() Settings {
	s := Settings{CheckStore: true}
	s.Mat.Selector, s.Mat.ControlPort = "app.kubernetes.io/component=machine-a-tron", 1266
	return s
}

// DecodeSettings reads n into v over its defaults. An unknown key or a wrong type is refused, in fixed text that never
// repeats the YAML. So is a float anywhere, which yaml.v3 would truncate into an integer: quote a text value that looks
// like one.
func DecodeSettings(n yaml.Node, v any) error {
	var float func(*yaml.Node) bool
	float = func(n *yaml.Node) bool { return n.ShortTag() == "!!float" || slices.ContainsFunc(n.Content, float) }
	raw, err := yaml.Marshal(&n)
	if float(&n) {
		err = errors.New("a float")
	}
	if err == nil {
		dec := yaml.NewDecoder(bytes.NewReader(raw))
		dec.KnownFields(true)
		err = dec.Decode(v)
	}
	if err != nil {
		return &config.Error{Field: "settings", Reason: "unknown key, wrong value type or bad YAML"}
	}
	return nil
}

// Nico is the nico-api client settings: the mounted certificate, its SPIFFE rules and the server name.
func (s Settings) Nico() forge.Settings {
	n := s.NicoAPI
	return forge.Settings{CertDir: cmp.Or(n.CertDir, forge.DefaultCertDir), Spiffe: n.Spiffe, ServerName: n.ServerName}
}

// Production wires the live site: the probe, the MAT pods and Services through kube, the MAT records over HTTPS,
// nico-api through the mounted client certificate, each BMC at its own IP and, when checked, the metrics store.
func Production(cfg *config.Config, s Settings, kube *cluster.Kube) (readiness.Deps, error) {
	p, err := probe.NewHTTP(cfg.Probe.URL, time.Duration(cfg.Probe.RequestTimeoutSeconds)*time.Second)
	if err != nil {
		return readiness.Deps{}, err
	}
	nico := s.Nico()
	d := readiness.Deps{Probe: p, Pods: kube, Records: inventory.NewHTTPReader(), Services: kube,
		Identity: func() (string, error) {
			crt := filepath.Join(nico.CertDir, "tls.crt")
			if err := forge.CheckKeyPairFiles(crt, filepath.Join(nico.CertDir, "tls.key")); err != nil {
				return "", err
			}
			cert, err := os.ReadFile(crt)
			if err != nil {
				return "", errors.New(crt + ": could not be read")
			}
			return forge.CheckIdentity(cert, nico.Spiffe)
		},
		Version: func(ctx context.Context, apiURL string) (string, error) {
			c, err := forge.Dial(apiURL, nico)
			if err != nil {
				return "", err
			}
			defer func() { _ = c.Close() }()
			return c.Version(ctx)
		},
		Dialer: bmc.DirectDialer{Port: s.Bmc.WithDefaults().Port},
		Settings: readiness.Settings{Namespace: cfg.Namespace, MatSelector: s.Mat.Selector, MatPort: s.Mat.ControlPort,
			Expected: s.Expected, Release: s.ExpectedRelease, BMC: s.Bmc, ControlPlane: cfg.ControlPlane},
	}
	if !s.CheckStore {
		return d, nil
	}
	// The store client is pinned to the series metadata the probe serves, read in the run, and so are its age bounds:
	// two scrape or family reader intervals plus the ingestion delay. An interval is capped at a day, the store's
	// longest window, so the sum cannot overflow.
	d.Store = func(ctx context.Context) (readiness.StoreProbe, error) {
		c, err := p.Config(ctx, probe.ConfigFilter{})
		switch {
		case err != nil:
			return readiness.StoreProbe{}, fmt.Errorf("probe metrics metadata: %w", err)
		case c.Metrics == nil:
			return readiness.StoreProbe{}, errors.New("the probe declares no metrics store")
		}
		meta, err := history.NewProbeMetadata(*c.Metrics)
		if err != nil {
			return readiness.StoreProbe{}, fmt.Errorf("probe metrics metadata: %w", err)
		}
		client, err := store.New(s.Store, meta, store.DefaultOptions)
		if err != nil {
			return readiness.StoreProbe{}, err
		}
		delay := time.Duration(s.Store.IngestionDelaySeconds) * time.Second
		bound := func(interval time.Duration) time.Duration { return 2*min(interval, 24*time.Hour) + delay }
		sp := readiness.StoreProbe{Client: client, ScrapeBound: bound(meta.ScrapeInterval), SweepBounds: map[probeapi.Family]time.Duration{}}
		for f, interval := range meta.ReaderIntervals {
			sp.SweepBounds[f] = bound(interval)
		}
		return sp, nil
	}
	return d, nil
}

// Test is the fleet-up run. check is readiness.Run; a test swaps it.
type Test struct {
	cfg   *config.Config
	deps  readiness.Deps
	check func(context.Context, readiness.Deps, time.Time) readiness.Report
}

func New(cfg *config.Config, d readiness.Deps) *Test { return &Test{cfg, d, readiness.Run} }

func (*Test) Name() string              { return "fleet-up" }
func (*Test) LockKind() lock.HolderKind { return lock.HolderFleetUp }
func (*Test) Writes() bool              { return false }

// Run checks the site by the readiness deadline: the readiness timeout, cut to the run's last read, less a tenth of the
// timeout, at most 5 s, so the verdict is in before either. A check cut short is a notReady reason, not an abort. It is go when every check passed, else notReady. Each failed check is a reason, and the
// report, a pass's detail included, is the result's detail. A cancel is returned so the run aborts.
func (t *Test) Run(rc run.RunCtx, _ *writer.Writer) (result.Outcome, error) {
	window := time.Duration(t.cfg.Timeouts.ReadinessSeconds) * time.Second
	d := t.deps
	d.Now = rc.Clock.Now
	until := rc.Clock.Instant().Add(window)
	if rc.ProbeReadEnd.Before(until) { // the claim and the lock may have taken most of the window
		until = rc.ProbeReadEnd
	}
	report := t.check(rc.Ctx, d, until.Add(-min(window/10, 5*time.Second)))
	if err := rc.Ctx.Err(); err != nil {
		return result.Outcome{}, err
	}
	reasons := []string{}
	for _, c := range report.Checks {
		if !c.OK {
			reasons = append(reasons, c.Name+": "+c.Detail)
		}
	}
	verdict := result.NotReady
	if report.Ready() {
		verdict = result.Go
	}
	return result.Found(verdict, reasons, report.Detail()), nil
}
