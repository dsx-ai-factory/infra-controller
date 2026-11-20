// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"math"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/history"
)

func TestFirmwareVerification(t *testing.T) {
	for _, mode := range []string{"valid", "counter newer", "cap newer", "heartbeat only", "missing cap", "missing identity", "duplicate pod", "stale", "stale cap", "gap", "reset", "NaN", "fractional"} {
		t.Run(mode, func(t *testing.T) {
			now := time.Unix(180, 0)
			row := func(name string) history.RawSeries {
				return history.RawSeries{Labels: map[string]string{"__name__": name, "namespace": "core", "pod": "api-0"}, Samples: []history.Sample{{AtMs: now.Add(-30 * time.Second).UnixMilli(), Value: 7}, {AtMs: now.UnixMilli(), Value: 9}}}
			}
			rows := []history.RawSeries{row(firmwarePassCounter), row(firmwareCapGauge)}
			switch mode {
			case "counter newer", "cap newer":
				i := 1
				if mode == "cap newer" {
					i = 0
				}
				for j := range rows[i].Samples {
					rows[i].Samples[j].AtMs -= 30000
				}
			case "heartbeat only":
				rows[0].Labels["__name__"] = "carbide_managed_loop_iterations_total"
			case "missing cap":
				rows = rows[:1]
			case "missing identity":
				for i := range rows {
					delete(rows[i].Labels, "pod")
				}
			case "duplicate pod":
				rows = append(rows, row(firmwarePassCounter))
			case "stale":
				for i := range rows {
					rows[i].Samples[1].AtMs = now.Add(-32 * time.Second).UnixMilli()
					rows[i].Samples[0].AtMs = now.Add(-62 * time.Second).UnixMilli()
				}
			case "stale cap":
				for i := range rows[1].Samples {
					rows[1].Samples[i].AtMs -= 32000
				}
			case "gap":
				rows[0].Samples[0].AtMs = now.Add(-32 * time.Second).UnixMilli()
			case "reset":
				rows[0].Samples[1].Value = 0
			case "NaN":
				rows[0].Samples[1].Value = math.NaN()
			case "fractional":
				rows[0].Samples[1].Value = 7.5
			}
			got, err := firmwareManagers(rows, now, 30*time.Second)
			if (err == nil) != (mode == "valid" || mode == "counter newer" || mode == "cap newer") {
				t.Fatalf("got=%v err=%v", got, err)
			}
			if err == nil {
				m := got["core/api-0"]
				if m.At.UnixMilli() != rows[0].Samples[1].AtMs || m.Passes != uint64(rows[0].Samples[1].Value) {
					t.Fatalf("counter evidence detached: %+v", m)
				}
				if mode == "counter newer" && time.Unix(165, 0).After(m.At) {
					t.Fatal("a sweep before the counter proof was accepted")
				}
			}
		})
	}
}

func TestFirmwareManagerCadence(t *testing.T) {
	for _, mode := range []string{"probe 5 manager 30", "gap", "stale", "long poll", "long poll gap", "delayed", "delayed stale", "delayed partial"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				cfg := Config{}
				if strings.HasPrefix(mode, "delayed") {
					cfg.IngestionDelaySeconds = 5
				}
				s := newSite(t, DefaultOptions, cfg) // probe metadata advertises 5 s
				ats, seconds, since := []float64{20, 50, 80}, 90, time.Time{}
				if cfg.IngestionDelaySeconds > 0 {
					ats, seconds = []float64{0, 30, 60}, 94
				}
				if strings.HasPrefix(mode, "long poll") {
					ats = []float64{20, 50, 80, 110, 140, 170}
					seconds = 180
					since = time.UnixMilli(ms(50))
				}
				for _, at := range ats {
					if mode == "gap" && at == 50 || mode == "stale" && at == 80 || mode == "long poll gap" && at == 110 || mode == "delayed stale" && at == 60 {
						continue
					}
					for _, name := range []string{firmwarePassCounter, firmwareCapGauge} {
						s.store.Add(name, ms(at), at, "namespace", "core", "pod", "api-0", "job", "api")
					}
				}
				if mode == "delayed partial" {
					s.store.Add(firmwarePassCounter, ms(90), 90, "namespace", "core", "pod", "api-0", "job", "api")
				}
				time.Sleep(time.Duration(seconds) * time.Second)
				got, err := s.client.FirmwareManagers(context.Background(), time.Now().Add(time.Minute), map[string]string{"job": "api"}, 30*time.Second, since)
				if (err == nil) != (mode == "probe 5 manager 30" || mode == "long poll" || mode == "delayed" || mode == "delayed partial") {
					t.Fatalf("managers=%v err=%v", got, err)
				}
				if err == nil && (got["core/api-0"].At.UnixMilli() != ms(ats[len(ats)-1]) || got["core/api-0"].From.UnixMilli() != ms(ats[0])) {
					t.Fatal("sample coverage lost", got)
				}
			})
		})
	}
}
