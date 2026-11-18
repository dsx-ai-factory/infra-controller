// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package history

import (
	"context"
	"math"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

func TestFirmwareTiming(t *testing.T) {
	for _, mode := range []string{"valid nonzero", "long config", "long host", "end-aged config", "end-aged host", "instant reads", "stale host", "future config", "future host", "missing count", "NaN count", "fractional count", "current revision", "pinned revision", "missing revision", "failed host attempt", "failed config attempt", "stale config", "inverted config", "missing attempt", "count reset", "scrape gap"} {
		t.Run(mode, func(t *testing.T) {
			f := probeapi.Filter{Type: probeapi.UnitTypeHost, Vendor: new("NVIDIA"), Model: new("GB200 NVL"), FirmwareComponent: new(probeapi.ComponentBmc), FirmwarePhase: new(probeapi.FirmwarePhaseCurrent), FirmwareReady: new(true)}
			spec, err := probeapi.SeriesSpecFor(f, false)
			if err != nil {
				t.Fatal(err)
			}
			r := &Request{Spec: spec, in: input{process: testProcess, interval: 30, firmwareRevision: 7, scrape: 15}}
			at := int64(t0 * 1000)
			row := func(name string, v float64, labels ...string) RawSeries {
				l := map[string]string{"__name__": name, "job": "probe"}
				for i := 0; i < len(labels); i += 2 {
					l[labels[i]] = labels[i+1]
				}
				return RawSeries{Labels: l, Samples: []Sample{{AtMs: at, Value: v}}}
			}
			s := Samples{Counts: []RawSeries{row(probeapi.Prefix+"hosts_firmware", 5, "vendor", "NVIDIA", "model", "GB200 NVL", "component", "bmc", "phase", "current", "ready", "true")}, Times: []RawSeries{row(sweepStart, t0-5, "reader", "devices", "family", "host"), row(sweepEnd, t0-4, "reader", "devices", "family", "host"), row(processStart, testProcess)}, Caps: [][]RawSeries{{
				row(probeapi.Prefix+"firmware_config_revision", 7), row(probeapi.Prefix+"host_firmware_config_revision", 7), row(sweepStart, t0-8, "reader", "nico_config", "family", "config_nico_api"), row(sweepEnd, t0-6, "reader", "nico_config", "family", "config_nico_api"), row(probeapi.Prefix+"last_attempt_failed", 1, "reader", "devices", "family", "host", "kind", "none"), row(probeapi.Prefix+"last_attempt_failed", 1, "reader", "nico_config", "family", "config_nico_api", "kind", "none"),
			}}}
			switch mode {
			case "missing count":
				s.Counts = nil
			case "NaN count":
				s.Counts[0].Samples[0].Value = math.NaN()
			case "fractional count":
				s.Counts[0].Samples[0].Value = 5.5
			case "current revision":
				s.Caps[0][0].Samples[0].Value = 8
			case "pinned revision":
				s.Caps[0][1].Samples[0].Value = 8
			case "missing revision":
				s.Caps[0] = s.Caps[0][1:]
			case "failed host attempt":
				s.Caps[0][4].Samples[0].Value = 0
			case "failed config attempt":
				s.Caps[0][5].Samples[0].Value = 0
			case "long config":
				s.Caps[0][2].Samples[0].Value = t0 - 301
				s.Caps[0][3].Samples[0].Value = t0 - 1
			case "long host":
				s.Times[0].Samples[0].Value = t0 - 301
				s.Times[1].Samples[0].Value = t0 - 1
			case "end-aged config":
				s.Caps[0][2].Samples[0].Value = t0 - 75
				s.Caps[0][3].Samples[0].Value = t0 - 55
			case "end-aged host":
				s.Times[0].Samples[0].Value = t0 - 75
				s.Times[1].Samples[0].Value = t0 - 55
			case "instant reads":
				s.Times[0].Samples[0].Value = s.Times[1].Samples[0].Value
				s.Caps[0][2].Samples[0].Value = s.Caps[0][3].Samples[0].Value
			case "stale config":
				s.Caps[0][2].Samples[0].Value = t0 - 361
				s.Caps[0][3].Samples[0].Value = t0 - 61
			case "stale host":
				s.Times[0].Samples[0].Value = t0 - 361
				s.Times[1].Samples[0].Value = t0 - 61
			case "future config":
				s.Caps[0][3].Samples[0].Value = t0 + 2
			case "future host":
				s.Times[1].Samples[0].Value = t0 + 2
			case "inverted config":
				s.Caps[0][2].Samples[0].Value = t0 - 1
			case "missing attempt":
				s.Caps[0] = s.Caps[0][:5]
			}
			if mode == "count reset" || mode == "scrape gap" {
				delta := int64(15000)
				if mode == "scrape gap" {
					delta = 17000
				}
				add := func(rows []RawSeries) {
					for i := range rows {
						v := rows[i].Samples[0]
						v.AtMs += delta
						if mode == "count reset" && rows[i].Labels["__name__"] == probeapi.Prefix+"hosts_firmware" {
							v.Value = 4
						}
						rows[i].Samples = append(rows[i].Samples, v)
					}
				}
				add(s.Counts)
				add(s.Times)
				add(s.Caps[0])
			}
			points, err := r.points(s)
			if mode == "valid nonzero" || mode == "end-aged config" || mode == "end-aged host" || mode == "instant reads" {
				if err != nil || len(points) != 1 || points[0].count != 5 {
					t.Fatalf("points=%+v err=%v", points, err)
				}
			} else if err == nil {
				t.Fatal("unsafe firmware samples accepted")
			}
		})
	}
}

func TestFirmwareTimingContract(t *testing.T) {
	f := probeapi.Filter{Type: probeapi.UnitTypeHost, FirmwareComponent: new(probeapi.ComponentBmc), FirmwareReady: new(true)}
	for _, mode := range []string{"missing revision", "mismatched cadence", "valid"} {
		t.Run(mode, func(t *testing.T) {
			meta := ProbeMetadata{FirmwareRevision: 7, Info: probeapi.MetricsInfo{ProcessStartSeconds: testProcess}, ReaderIntervals: map[probeapi.Family]time.Duration{probeapi.FamilyHost: 30 * time.Second, probeapi.FamilyConfigNicoAPI: 30 * time.Second}}
			if mode == "missing revision" {
				meta.FirmwareRevision = 0
			}
			if mode == "mismatched cadence" {
				meta.ReaderIntervals[probeapi.FamilyConfigNicoAPI] = time.Minute
			}
			_, err := Prepare(context.Background(), Context{Probe: meta, Now: func() time.Time { return time.Unix(int64(t0+100), 0) }, Until: time.Now().Add(time.Minute)}, f, probeapi.SummaryParams{Start: jsonx.Time{Time: time.Unix(int64(t0), 0)}, End: jsonx.Time{Time: time.Unix(int64(t0+60), 0)}, N: 1}, time.Hour)
			if (err == nil) != (mode == "valid") {
				t.Fatal(err)
			}
		})
	}
	spec, err := probeapi.SeriesSpecFor(f, false)
	if err != nil || spec.Metric != "hosts_firmware" || spec.Matchers[0] != [2]string{"component", "bmc"} {
		t.Fatalf("%+v %v", spec, err)
	}
	f.Source = new(probeapi.SourceRest)
	if _, err = probeapi.SeriesSpecFor(f, false); err == nil {
		t.Fatal("REST firmware count")
	}
}
