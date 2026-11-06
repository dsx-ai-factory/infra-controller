// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"errors"
	"math"
	"net/http"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/history"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

// base is where a synctest bubble's clock starts, in Unix seconds, and
// process the start of the test probe's process, 100,000 s before it.
const (
	base    = 946_684_800.0
	process = base - 100_000
)

var ready = probeapi.Filter{Type: probeapi.UnitTypeHost, Stage: new(probeapi.StageReady)}

func info() probeapi.MetricsInfo {
	return probeapi.MetricsInfo{
		ContractVersion: probeapi.ContractVersion, Selector: map[string]string{"job": "j", "namespace": "ns"}, ScrapeIntervalSeconds: 5,
		ReaderIntervalsSeconds: map[probeapi.Family]float64{probeapi.FamilyHost: 20, probeapi.FamilyRestTotals: 120}, ProcessStartSeconds: process,
	}
}

func metadata(t *testing.T) history.ProbeMetadata {
	m, err := history.NewProbeMetadata(info())
	if err != nil {
		t.Fatal(err)
	}
	return m
}

// site is a client over a fake store, in a synctest bubble whose clock
// started at base. Offsets are seconds from base.
type site struct {
	t      *testing.T
	store  *fakeStore
	client *Client
}

func newSite(t *testing.T, o Options, cfg Config) *site {
	s := &site{t: t, store: &fakeStore{}}
	cfg.URL = "http://store"
	var err error
	if s.client, err = New(cfg, metadata(t), o); err != nil {
		t.Fatal(err)
	}
	s.client.http.Transport = fakeTransport{Handler: s.store}
	return s
}

func ms(offset float64) int64 { return int64(math.Round((base + offset) * 1000)) }

var selector = []string{"job", "j", "namespace", "ns"}

// scrapeVisible records one scrape at at of a probe whose host sweep ended at
// end with count ready hosts, queryable from visible on, src added to selector.
func (s *site) scrapeVisible(at, count, end, visible float64, src ...string) {
	src = append(src, selector...)
	add := func(name string, v float64, kv ...string) {
		s.store.AddLate(name, ms(at), ms(visible), v, append(kv, src...)...)
	}
	add(probeapi.Prefix+"units", count, "type", "host", "stage", "ready", "state", "Ready")
	add(probeapi.MetricSweepComplete, base+end, "reader", "devices", "family", "host")
	add(probeapi.MetricSweepStart, base+end-10, "reader", "devices", "family", "host")
	add(probeapi.MetricProcessStart, process)
}

func (s *site) scrape(at, count, end float64) { s.scrapeVisible(at, count, end, at) }

func params(start, end float64, n uint64) probeapi.SummaryParams {
	return probeapi.SummaryParams{Start: history.AtSecs(base + start), End: history.AtSecs(base + end), N: n}
}

func (s *site) summary(f probeapi.Filter, p probeapi.SummaryParams) (probeapi.SummaryResponse, error) {
	return s.client.Summarize(s.t.Context(), time.Now().Add(time.Hour), f, p)
}

func reached(r probeapi.SummaryResponse) string {
	if len(r.Crossings) == 0 {
		return "-"
	}
	return opt(r.Crossings[0].ReachedAt)
}

func at(offset float64) string { return opt(new(history.AtSecs(base + offset))) }

func bubble(t *testing.T, test func(t *testing.T)) {
	t.Helper()
	synctest.Test(t, test)
}

func TestNewRefusesAnInfeasibleConfig(t *testing.T) {
	day := 24 * time.Hour
	for _, c := range []struct {
		o     Options
		delay uint64
		ok    bool
	}{
		{DefaultOptions, 0, true},
		{Options{day, 20 * time.Second}, 59, false}, // an ingestion delay at or over half the timeout
		{Options{0, 90 * time.Second}, 0, false},
		{Options{day, 0}, 0, false},
		{DefaultOptions, 44, true},
		{DefaultOptions, 45, false},
		{Options{day, math.MaxInt64}, 60, true},
	} {
		_, err := New(Config{URL: "http://store", IngestionDelaySeconds: c.delay}, metadata(t), c.o)
		eq(t, err == nil, c.ok, c.o, c.delay, err)
	}
}

func TestSummaries(t *testing.T) {
	// Sweeps every 20 s, scraped every 5 s, until the clock, and the next
	// scrape 7 s after the window's end.
	readyHosts := func(s *site) {
		for i := range 24 {
			at := -120 + 5*float64(i)
			end := math.Floor(at/20)*20 - 1
			s.scrape(at, map[bool]float64{true: 10, false: 0}[end >= -10], end)
		}
		s.scrape(7, 10, -1)
	}
	for _, c := range []struct {
		name  string
		o     Options
		cfg   Config
		build func(*site)
		p     probeapi.SummaryParams
		check func(t *testing.T, s *site, r probeapi.SummaryResponse, err error, took time.Duration)
	}{
		{"the longest window reads in half the deadline", DefaultOptions, Config{}, func(s *site) { s.scrape(0, 1, -1) }, params(-86_400, 0, 1),
			func(t *testing.T, _ *site, r probeapi.SummaryResponse, err error, took time.Duration) {
				eq(t, []any{err, r.FinalScrapeSeen, took <= DefaultOptions.SummaryTimeout/2}, []any{nil, true, true}, took)
			}},
		{"waits for the final scrape, then counts the tail", DefaultOptions, Config{}, readyHosts, params(-100, 0, 10),
			func(t *testing.T, _ *site, r probeapi.SummaryResponse, err error, _ time.Duration) {
				eq(t, []any{err, r.FinalScrapeSeen, opt(r.FinalCount), reached(r), opt(r.MaxSweepGapSeconds)}, []any{nil, true, "10", at(-1), "20"})
			}},
		// Behind a 30 s ingestion delay the summary waits that much longer for the
		// final scrape and reads the window's last 30 s, which it first could not.
		{"an ingestion delay delays the final scrape and the tail", DefaultOptions, Config{IngestionDelaySeconds: 30},
			func(s *site) { s.store.SetDelay(0, 30*time.Second); readyHosts(s) }, params(-100, 0, 10),
			func(t *testing.T, _ *site, r probeapi.SummaryResponse, err error, took time.Duration) {
				eq(t, []any{err, took >= 37*time.Second, opt(r.FinalCount), reached(r), opt(r.MaxSweepGapSeconds), r.SweepsSeen},
					[]any{nil, true, "10", at(-1), "20", uint32(5)}, took)
			}},
		// Within the ingestion delay a scrape may have landed in part: its sweep
		// times at once, its count later. No read goes nearer to now than the delay.
		{"reads stop where every scrape has landed", DefaultOptions, Config{IngestionDelaySeconds: 10}, func(s *site) {
			for i := range 24 {
				at := -120 + 5*float64(i)
				s.scrape(at, 0, math.Floor(at/20)*20-1)
			}
			s.store.AddLate(probeapi.MetricSweepComplete, ms(7), ms(7), base-1, append([]string{"reader", "devices", "family", "host"}, selector...)...)
			s.store.AddLate(probeapi.MetricSweepStart, ms(7), ms(7), base-11, append([]string{"reader", "devices", "family", "host"}, selector...)...)
			s.store.AddLate(probeapi.MetricProcessStart, ms(7), ms(7), process, selector...)
			s.store.AddLate(probeapi.Prefix+"units", ms(7), ms(11), 10, append([]string{"type", "host", "stage", "ready", "state", "Ready"}, selector...)...)
		}, params(-100, 0, 10),
			func(t *testing.T, _ *site, r probeapi.SummaryResponse, err error, _ time.Duration) {
				eq(t, []any{err, r.FinalScrapeSeen, opt(r.FinalCount), reached(r)}, []any{nil, true, "10", at(-1)})
			}},
		// A store that keeps one sample per 10 s keeps every other 5 s
		// scrape. With 20 s sweeps every sweep is still seen.
		{"deduplicated scrapes keep the crossings", DefaultOptions, Config{}, func(s *site) {
			for i := 1; i < 24; i += 2 {
				at := -120 + 5*float64(i)
				s.scrape(at, 0, math.Floor(at/20)*20-1)
			}
			s.scrape(7, 7, -1)
		}, params(-100, 0, 7),
			func(t *testing.T, _ *site, r probeapi.SummaryResponse, err error, _ time.Duration) {
				eq(t, []any{err, reached(r), r.FinalScrapeSeen, opt(r.FinalCount)}, []any{nil, at(-1), true, "7"})
			}},
		// A run two hours ago, its final scrape 3 s after its end, and an unrelated
		// restart an hour after it: no wait, no read past the post-end span.
		{"a historical final poll reads only post-end evidence", DefaultOptions, Config{}, func(s *site) {
			s.scrape(-7_260, 1, -7_265)
			s.scrape(-7_197, 2, -7_205)
			times := append([]string{"reader", "devices", "family", "host"}, selector...)
			s.store.Add(probeapi.Prefix+"units", ms(-3_600), 99, append([]string{"type", "host", "stage", "ready", "state", "Ready"}, selector...)...)
			s.store.Add(probeapi.MetricSweepComplete, ms(-3_600), base-3_600, times...)
			s.store.Add(probeapi.MetricSweepStart, ms(-3_600), base-3_605, times...)
			s.store.Add(probeapi.MetricProcessStart, ms(-3_600), base-3_660, selector...)
		}, params(-7_250, -7_200, 2),
			func(t *testing.T, s *site, r probeapi.SummaryResponse, err error, took time.Duration) {
				// No poll waits: the time is the gate's spacing of four calls.
				eq(t, []any{err, took, r.FinalScrapeSeen, opt(r.FinalCount)}, []any{nil, 1500 * time.Millisecond, true, "2"})
				for _, q := range s.store.Queries() {
					span := strings.TrimSuffix(q.PromQL[strings.LastIndex(q.PromQL, "[")+1:], "]")
					d, perr := time.ParseDuration(span)
					if q.AtMs > ms(-7_200+15) || perr != nil || d > 3600*time.Second {
						t.Errorf("%s at %d", q.PromQL, q.AtMs)
					}
				}
			}},
		// The final scrape lands 4 s after the request: no query asks the
		// store about the future, and the poll reads up to now.
		{"the poll's upper bound is now", DefaultOptions, Config{}, func(s *site) { s.scrape(-20, 1, -25); s.scrape(4, 2, 2) }, params(-60, 0, 1),
			func(t *testing.T, s *site, r probeapi.SummaryResponse, err error, _ time.Duration) {
				last := int64(0)
				for _, q := range s.store.Queries() {
					last = max(last, q.AtMs)
				}
				eq(t, []any{err, r.FinalScrapeSeen, opt(r.FinalCount), last >= ms(4)}, []any{nil, true, "1", true})
			}},
		{"a summary over five seconds answers", DefaultOptions, Config{}, func(s *site) {
			s.scrape(-10_000, 1, -10_005)
			s.scrape(0, 2, -5)
			s.store.SetDelay(2*time.Second, 0)
		}, params(-10_000, 0, 2),
			func(t *testing.T, _ *site, r probeapi.SummaryResponse, err error, took time.Duration) {
				eq(t, []any{err, took > 5*time.Second, opt(r.FinalCount)}, []any{nil, true, "2"})
			}},
		{"the longest summary timeout never overflows", Options{24 * time.Hour, math.MaxInt64}, Config{}, func(s *site) { s.scrape(0, 1, -5) }, params(-60, 0, 1),
			func(t *testing.T, _ *site, r probeapi.SummaryResponse, err error, _ time.Duration) {
				eq(t, []any{err, opt(r.FinalCount)}, []any{nil, "1"})
			}},
		{"the summary timeout is a timeout", Options{time.Hour, 10 * time.Second}, Config{}, func(s *site) { s.store.SetDelay(4*time.Second, 0) }, params(-600, 0, 1),
			func(t *testing.T, s *site, _ probeapi.SummaryResponse, err error, took time.Duration) {
				s.store.SetDelay(0, 0)
				status, statusErr := s.client.StoreStatus(t.Context(), time.Now().Add(time.Minute))
				eq(t, []any{err, took, statusErr, status.LastSampleAt}, []any{error(history.Timeout{}), 10 * time.Second, nil, (*jsonx.Time)(nil)})
			}},
		{"a filter of a family no reader sweeps", DefaultOptions, Config{}, func(*site) {}, params(-60, 0, 1),
			func(t *testing.T, s *site, _ probeapi.SummaryResponse, _ error, _ time.Duration) {
				_, err := s.summary(probeapi.Filter{Type: probeapi.UnitTypeDpu}, params(-60, 0, 1))
				eq(t, err, error(history.Unsupported{Text: "the probe reads no dpu sweeps"}))
			}},
		// The only zero baseline is scraped exactly at the range's excluded lower
		// bound (two reader intervals before the start); the read must include it.
		{"a baseline at the exact lower bound is read", DefaultOptions, Config{},
			func(s *site) { s.scrape(-100, 0, -100); s.scrape(-30, 2, -30) }, params(-60, 0, 2),
			func(t *testing.T, _ *site, r probeapi.SummaryResponse, err error, _ time.Duration) {
				eq(t, []any{err, opt(r.FinalCount), reached(r)}, []any{nil, "2", at(-30)})
			}},
	} {
		t.Run(c.name, func(t *testing.T) {
			bubble(t, func(t *testing.T) {
				s := newSite(t, c.o, c.cfg)
				c.build(s)
				started := time.Now()
				r, err := s.summary(ready, c.p)
				c.check(t, s, r, err, time.Since(started))
			})
		})
	}
}

// A whole six-hour run window, its two-reader-interval baseline lookback and its
// three-scrape final-scrape lookahead fit one range query per series under the
// reply byte cap and the cumulative sample budget, repeated polls included: it
// succeeds, which proves the window fits.
func TestSixHourWindowFitsTheStoreLimits(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := newSite(t, DefaultOptions, Config{IngestionDelaySeconds: 10})
		s.store.SetDelay(0, 10*time.Second)
		const win = 6 * 3600.0
		count := func(end float64) float64 {
			if end < -win {
				return 0 // the baseline before the window proves zero
			}
			return min(math.Floor((end+win)/(win/4500)), 4500)
		}
		// Two replicas scrape every 5 s from before the window to just after it; the
		// final scrape is visible only after the 10 s delay, so the poll re-reads it.
		for at := -win - 50.0; at <= 5; at += 5 {
			end := at - 5
			for i, rep := range []string{"a", "b"} {
				s.scrapeVisible(at+float64(i)*2, count(end), end, at+float64(i)*2, "instance", rep)
			}
		}
		started := time.Now()
		r, err := s.summary(ready, params(-win, 0, 4500))
		eq(t, []any{err, r.FinalScrapeSeen, opt(r.FinalCount), len(s.store.Queries()) > 2, time.Since(started) < DefaultOptions.SummaryTimeout},
			[]any{nil, true, "4500", true, true}, len(s.store.Queries()), time.Since(started))
	})
}

func opt[T any](p *T) string {
	if p == nil {
		return "-"
	}
	return strings.Trim(jsonText(*p), `"`)
}

func jsonText(v any) string {
	b, _ := jsonx.Encode(v)
	return string(b)
}

// The final scrape is polled for: a run ending 0.3 s before the request, its
// sweeps scraped before the end, sees its final scrape and count 2, however late.
func TestFinalPoll(t *testing.T) {
	for _, c := range []struct {
		name  string
		after func(*site)
		end   float64
		want  string
	}{
		// The only scrape after the end, 0.25 s after it, lands 4 s after the
		// request: a poll that read its time before then reads it again.
		{"a late scrape is read again", func(s *site) { s.scrapeVisible(-0.05, 2, -5.3, 4) }, -0.3, at(-5.3)},
		// The only scrape after the end lands between two polls.
		{"an offset scrape", func(s *site) { s.scrape(2.75, 2, -5.3) }, -0.3, at(-5.3)},
		// Review fix: a scrape just before the end lands after the window was
		// read, and shows the only in-window sweep that reached the target.
		{"a late scrape from before the end", func(s *site) { s.scrapeVisible(-0.5, 2, -1, 2); s.scrape(4.5, 2, 4) }, 0, at(-1)},
		// The first scraper's scrape just before the end holds the crossing and lands
		// late; polls go on until it lands, past the second's later sweep after the end.
		{"another scraper's late scrape", func(s *site) {
			s.scrapeVisible(-0.5, 2, -1, 2, "instance", "a")
			s.scrapeVisible(0.25, 2, 0.1, 0.25, "instance", "b")
			time.Sleep(300 * time.Millisecond)
		}, 0, at(-1)},
	} {
		bubble(t, func(t *testing.T) {
			s := newSite(t, DefaultOptions, Config{})
			// A zero baseline before the window, then no ready host until the
			// late scrape: the crossing is only in that scrape.
			s.scrape(-65.3, 0, -70)
			s.scrape(-40.3, 0, -45)
			s.scrape(-20.3, 0, -25)
			c.after(s)
			r, err := s.summary(ready, params(-60.3, c.end, 2))
			fractional := false
			for _, q := range s.store.Queries() {
				fractional = fractional || strings.HasSuffix(q.PromQL, "ms]")
				if q.AtMs > ms(c.end+15) {
					t.Errorf("%s: read past the horizon: %d", c.name, q.AtMs)
				}
			}
			eq(t, []any{err, r.FinalScrapeSeen, opt(r.FinalCount), reached(r), fractional || c.end == 0}, []any{nil, true, "2", c.want, true}, c.name)
		})
	}
}

// A whole scrape landing between a poll's two reads is counted consistently:
// sweep times are read before counts, so a final sweep a poll sees has its
// count by the read after it. Injected before the settling poll's second read,
// it fails this test if counts were read first (its count dropped).
func TestAWholeScrapeArrivesBetweenAPollsReads(t *testing.T) {
	bubble(t, func(t *testing.T) {
		s := newSite(t, DefaultOptions, Config{})
		// A fresh zero baseline, then no ready host until the final scrape lands late in the third poll.
		s.scrape(-65.3, 0, -70)
		var sawFirst, firstIsTimes, injected bool
		var starts int
		s.store.Hook = func(q fakeQuery) {
			isTimes := strings.Contains(q.PromQL, "sweep_start_timestamp_seconds")
			if !sawFirst {
				sawFirst, firstIsTimes = true, isTimes
			}
			if isTimes == firstIsTimes {
				starts++ // the first read of each fetch: the baseline, then each poll
				return
			}
			// The settling poll's second read, with the final scrape visible now.
			if starts == 4 && !injected {
				injected = true
				nowOff := float64(time.Now().UnixMilli()-ms(0)) / 1000
				s.scrapeVisible(0, 2, 0, nowOff)
			}
		}
		r, err := s.summary(ready, params(-60.3, 0, 2))
		eq(t, []any{err, r.FinalScrapeSeen, opt(r.FinalCount), reached(r)}, []any{nil, true, "2", at(0)}, injected, len(s.store.Queries()))
	})
}

// Every hourly reply fits, but together they pass the sample budget: the
// summary is refused before it holds more. Half an hour fits.
func TestSampleBudgetSpansTheWholeRequest(t *testing.T) {
	bubble(t, func(t *testing.T) {
		s := newSite(t, DefaultOptions, Config{})
		s.client.budget = 3_000
		for i := range 1_440 {
			at := -7_195 + 5*float64(i)
			s.scrape(at, 1, at-1)
		}
		_, err := s.summary(ready, params(-7_000, 0, 1))
		var invalid history.InvalidInput
		eq(t, []any{errors.As(err, &invalid), invalid.Field, len(s.store.Queries()) <= 4}, []any{true, "start", true}, err)
		_, err = s.summary(ready, params(-1_800, 0, 1))
		eq(t, err, nil, "half an hour")
	})
}

func TestBusy(t *testing.T) {
	// A second summary is busy while the first one, with no sweeps, answers.
	bubble(t, func(t *testing.T) {
		s := newSite(t, DefaultOptions, Config{})
		s.store.SetDelay(4900*time.Millisecond, 0)
		var wg sync.WaitGroup
		errs := make([]error, 2)
		for i := range errs {
			wg.Go(func() { _, errs[i] = s.summary(ready, params(-600, 0, 1)) })
			time.Sleep(time.Millisecond)
		}
		wg.Wait()
		eq(t, errs, []error{nil, history.Busy{RetryAfter: busyRetryAfter}})
	})
	// Within a short deadline the wait ends at the deadline as busy, and the
	// summary in flight times out.
	bubble(t, func(t *testing.T) {
		s := newSite(t, Options{time.Hour, 10 * time.Second}, Config{})
		s.store.SetDelay(20*time.Second, 0)
		var wg sync.WaitGroup
		errs := make([]error, 2)
		started := time.Now()
		for i := range errs {
			wg.Go(func() { _, errs[i] = s.summary(ready, params(-600, 0, 1)) })
		}
		wg.Wait()
		busy := errs[0] == history.Busy{RetryAfter: busyRetryAfter} || errs[1] == history.Busy{RetryAfter: busyRetryAfter}
		eq(t, []any{busy, errs[0] == history.Timeout{} || errs[1] == history.Timeout{}, time.Since(started)}, []any{true, true, 10 * time.Second}, errs)
	})
	// A permit freed after the wait ended is still busy, and no store work
	// starts.
	bubble(t, func(t *testing.T) {
		s := newSite(t, DefaultOptions, Config{})
		s.client.permit <- struct{}{}
		done := make(chan error)
		go func() { _, err := s.summary(ready, params(-100, 0, 1)); done <- err }()
		time.Sleep(busyWait + time.Second)
		<-s.client.permit
		eq(t, []any{<-done, len(s.store.Queries())}, []any{error(history.Busy{RetryAfter: busyRetryAfter}), 0})
	})
}

func TestStoreStatus(t *testing.T) {
	bubble(t, func(t *testing.T) {
		s := newSite(t, DefaultOptions, Config{})
		status, err := s.client.StoreStatus(t.Context(), time.Now().Add(time.Minute))
		eq(t, []any{err, status}, []any{nil, Status{}}, "an empty store")
		s.scrape(-30, 1, -40)
		s.scrape(-12, 1, -20)
		status, err = s.client.StoreStatus(t.Context(), time.Now().Add(time.Minute))
		eq(t, []any{err, opt(status.LastSampleAt), opt(status.SampleAgeSeconds), opt(status.SweepCompleteAt)}, []any{nil, at(-12), "12", at(-20)})
		s.store.Add(probeapi.MetricSweepComplete, ms(-5), 1e300, append([]string{"reader", "devices", "family", "host"}, selector...)...)
		_, err = s.client.StoreStatus(t.Context(), time.Now().Add(time.Minute))
		eq(t, err, unavailableText("a time sample is out of range"))
	})
}

// No error holds store text, the URL's password or the store's address.
func TestStoreErrorsNeverCarryStoreText(t *testing.T) {
	for _, r := range []func(int32, http.ResponseWriter, *http.Request){
		reply(400, `{"status":"error","error":"`+sentinel+`"}`), reply(200, `{"status":"error","error":"`+sentinel+`"}`), reply(200, sentinel),
	} {
		bubble(t, func(t *testing.T) {
			c, _ := served(t, Config{URL: "http://store/prom"}, r)
			_, summaryErr := c.Summarize(t.Context(), time.Now().Add(time.Minute), ready, params(-600, -1, 1))
			_, statusErr := c.StoreStatus(t.Context(), time.Now().Add(time.Minute))
			for _, err := range []error{summaryErr, statusErr} {
				text := err.Error()
				eq(t, errors.As(err, new(history.Unavailable)) && !strings.Contains(text, sentinel) && !strings.Contains(text, "token") && !strings.Contains(text, "store/"), true, text)
			}
		})
	}
}

func TestPreflight(t *testing.T) {
	type sample struct {
		at, value float64
		src       string
	}
	for _, c := range []struct {
		name     string
		live     float64 // the process start the probe serves
		samples  []sample
		required []probeapi.Family
		want     string        // ok, unavailable or unsupported
		interval time.Duration // 0 keeps the default scrape interval
	}{
		{"an earlier process is history", process, []sample{{-100, process - 50, "a"}, {-10, process, "a"}, {-8, process, "b"}}, []probeapi.Family{probeapi.FamilyHost}, "ok", 0},
		{"another live process on a replica", process, []sample{{-10, process, "a"}, {-8, process + 1, "b"}}, nil, "unavailable", 0},
		{"nothing fresh", process, []sample{{-500, process, "a"}}, nil, "unavailable", 0},
		{"a family no reader sweeps", process, []sample{{-10, process, "a"}}, []probeapi.Family{probeapi.FamilyHost, probeapi.FamilyDpu}, "unsupported", 0},
		// The pod was replaced 30 s ago: the old pod's last sample predates
		// the live process. One still scraped after it is a second probe.
		{"a replaced pod is history", base - 30, []sample{{-60, base - 5_000, "old"}, {-10, base - 30, "new"}}, nil, "ok", 0},
		{"a replaced pod still scraped", base - 30, []sample{{-60, base - 5_000, "old"}, {-20, base - 5_000, "old"}, {-10, base - 30, "new"}}, nil, "unavailable", 0},
		// The engine binds by exact float bits, so a store that returns the process
		// start rounded is refused up front: only the identical value is the process.
		{"the exact process start", base - 100.1234567, []sample{{-10, base - 100.1234567, "a"}}, nil, "ok", 0},
		{"rounded down is unsupported", base - 100.1234567, []sample{{-10, base - 100.123, "a"}}, nil, "unavailable", 0},
		{"rounded up is unsupported", base - 100.1234567, []sample{{-10, base - 100.124, "a"}}, nil, "unavailable", 0},
		{"2 ms off", base - 100.1234567, []sample{{-10, base - 100.1255, "a"}}, nil, "unavailable", 0},
		// The lookback scales with the scrape interval (a 170 s sample seen at a 3 min
		// interval) and saturates at the max window when twice an accepted interval overflows.
		{"a slow scrape is still seen", process, []sample{{-170, process, "a"}}, nil, "ok", 3 * time.Minute},
		{"a huge interval saturates", process, []sample{{-3600, process, "a"}}, nil, "ok", 150 * 365 * 24 * time.Hour},
	} {
		bubble(t, func(t *testing.T) {
			s := newSite(t, DefaultOptions, Config{})
			s.client.meta.Info.ProcessStartSeconds = c.live
			if c.interval != 0 {
				s.client.meta.ScrapeInterval = c.interval
			}
			for _, x := range c.samples {
				s.store.Add(probeapi.MetricProcessStart, ms(x.at), x.value, append([]string{"pod", x.src}, selector...)...)
			}
			err := s.client.Preflight(t.Context(), time.Now().Add(time.Minute), c.required...)
			got := "ok"
			if errors.As(err, new(history.Unavailable)) {
				got = "unavailable"
			} else if errors.As(err, new(history.Unsupported)) {
				got = "unsupported"
			}
			eq(t, [2]any{got, err == nil}, [2]any{c.want, c.want == "ok"}, c.name, err)
		})
	}
}

// Review fix: the probe serves its process start in /v1/config JSON and the
// store returns it as text: both read back as the same float64.
func TestPreflightMatchesTheProcessStartThroughJSON(t *testing.T) {
	const text = "946684700.0000021"
	var probe probeapi.MetricsInfo
	err := jsonx.Decode([]byte(`{"contractVersion":1,"selector":{"job":"j","namespace":"ns"},"scrapeIntervalSeconds":5,`+
		`"readerIntervalsSeconds":{"host":20,"rest_totals":120},"processStartSeconds":`+text+`}`), &probe)
	eq(t, err, nil)
	bubble(t, func(t *testing.T) {
		s := newSite(t, DefaultOptions, Config{})
		s.client.meta.Info = probe
		s.client.http.Transport = fakeTransport{Handler: http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			_, _ = w.Write([]byte(matrix(probeapi.MetricProcessStart, `[[`+jsonText(base-10)+`,"`+text+`"]]`)))
		})}
		eq(t, s.client.Preflight(t.Context(), time.Now().Add(time.Minute)), nil)
	})
}

// Two different values for one series and time, in one reply or across the
// baseline and the final poll, are a store that cannot be trusted.
func TestConflictingSamplesFail(t *testing.T) {
	for _, c := range []struct {
		name string
		add  func(*site)
	}{
		{"count disagreement", func(s *site) {
			s.store.Add(probeapi.Prefix+"units", ms(-20), 2, append([]string{"type", "host", "stage", "ready", "state", "Ready"}, selector...)...)
		}},
		{"process disagreement", func(s *site) { s.store.Add(probeapi.MetricProcessStart, ms(-20), process+5, selector...) }},
		// A disagreement the window read did not see: 1 at once, the conflicting 2
		// only later in the poll's re-read span. Gathered samples persist across polls.
		{"across polls", func(s *site) {
			lbl := append([]string{"type", "host", "stage", "ready", "state", "Ready"}, selector...)
			s.store.Add(probeapi.Prefix+"units", ms(-2), 1, lbl...)
			s.store.AddLate(probeapi.Prefix+"units", ms(-2), ms(5), 2, lbl...)
		}},
	} {
		bubble(t, func(t *testing.T) {
			s := newSite(t, DefaultOptions, Config{})
			s.scrape(-65, 0, -70)
			s.scrape(-20, 1, -25)
			s.scrape(7, 1, -1)
			c.add(s)
			_, err := s.summary(ready, params(-60, 0, 1))
			eq(t, errors.As(err, new(history.Unavailable)), true, c.name, err)
		})
	}
}

// Process identity is exact float bits throughout. A store that returns the
// process start rounded is refused at preflight, so a green preflight never
// promises evidence the engine would reject; the exact value passes preflight
// and the summary then times the crossing.
func TestPreflightAndSummaryAgreeOnProcessIdentity(t *testing.T) {
	const live = base - 12345.6789012
	for _, exact := range []bool{true, false} {
		bubble(t, func(t *testing.T) {
			s := newSite(t, DefaultOptions, Config{})
			s.client.meta.Info.ProcessStartSeconds = live
			stored := live
			if !exact {
				stored = math.Round(live*1000) / 1000 // the store rounds to the millisecond
			}
			for _, x := range []struct{ at, count, end float64 }{{-65, 0, -70}, {-20, 1, -25}, {7, 1, -1}} {
				s.store.Add(probeapi.Prefix+"units", ms(x.at), x.count, append([]string{"type", "host", "stage", "ready", "state", "Ready"}, selector...)...)
				s.store.Add(probeapi.MetricSweepComplete, ms(x.at), base+x.end, append([]string{"reader", "devices", "family", "host"}, selector...)...)
				s.store.Add(probeapi.MetricSweepStart, ms(x.at), base+x.end-3, append([]string{"reader", "devices", "family", "host"}, selector...)...)
				s.store.Add(probeapi.MetricProcessStart, ms(x.at), stored, selector...)
			}
			perr := s.client.Preflight(t.Context(), time.Now().Add(time.Minute), probeapi.FamilyHost)
			if !exact {
				eq(t, errors.As(perr, new(history.Unavailable)), true, "rounded preflight refused", perr)
				return
			}
			eq(t, perr, nil, "exact preflight ok")
			r, err := s.summary(ready, params(-60, 0, 1))
			eq(t, []any{err, r.FinalScrapeSeen, reached(r)}, []any{nil, true, at(-25)}, "exact summary times the crossing")
		})
	}
}

// stuckDeadline has a deadline already past whose Done never closes, as a
// context whose timer callback has not run yet.
type stuckDeadline struct {
	context.Context
	at time.Time
}

func (c stuckDeadline) Deadline() (time.Time, bool) { return c.at, true }

// A caller's elapsed deadline is a timeout, never a cancellation, even when its
// Done has not fired, on the admission, summary, status and preflight paths.
func TestCallerDeadlineIsATimeout(t *testing.T) {
	for _, c := range []struct {
		name  string
		setup func(*site)
		call  func(ctx context.Context, s *site) error
	}{
		{"summary", func(*site) {}, func(ctx context.Context, s *site) error { return summarize(ctx, s, time.Now().Add(time.Hour)) }},
		{"admission", func(s *site) { s.client.permit <- struct{}{} }, func(ctx context.Context, s *site) error { return summarize(ctx, s, time.Now().Add(time.Hour)) }},
		{"store status", func(*site) {}, func(ctx context.Context, s *site) error {
			_, err := s.client.StoreStatus(ctx, time.Now().Add(time.Hour))
			return err
		}},
		{"preflight", func(*site) {}, func(ctx context.Context, s *site) error { return s.client.Preflight(ctx, time.Now().Add(time.Hour)) }},
	} {
		bubble(t, func(t *testing.T) {
			s := newSite(t, DefaultOptions, Config{})
			c.setup(s)
			ctx := stuckDeadline{t.Context(), time.Now().Add(-time.Second)}
			eq(t, c.call(ctx, s), error(history.Timeout{}), c.name)
		})
	}
}

// A real elapsed deadline, whose Done fires with context.DeadlineExceeded, is a
// timeout, not a cancellation: the store stalls past a 20 ms deadline.
func TestCallerDeadlineExceededIsATimeout(t *testing.T) {
	bubble(t, func(t *testing.T) {
		s := newSite(t, DefaultOptions, Config{})
		s.store.SetDelay(time.Minute, 0) // the store never answers in time
		ctx, cancel := context.WithTimeout(t.Context(), 20*time.Millisecond)
		defer cancel()
		started := time.Now()
		_, err := s.client.StoreStatus(ctx, time.Now().Add(time.Hour))
		eq(t, []any{err, time.Since(started)}, []any{error(history.Timeout{}), 20 * time.Millisecond})
	})
}

// A cancel or the deadline ends every wait: a store call, the in-flight wait,
// the final scrape poll, the store status, the preflight and the gate's spacing.
func TestCancelAndDeadlineBoundEveryWait(t *testing.T) {
	for _, c := range []struct {
		name  string
		setup func(*site)
		call  func(ctx context.Context, s *site, until time.Time) error
		after time.Duration // the cancel, 0 for none
		until time.Duration
		want  error
		took  time.Duration
	}{
		{"while the store answers", func(s *site) { s.store.SetDelay(time.Minute, 0) }, summarize, 3 * time.Second, time.Hour, history.Cancelled{}, 3 * time.Second},
		{"while waiting for the summary in flight", func(s *site) { s.client.permit <- struct{}{} }, summarize, 3 * time.Second, time.Hour, history.Cancelled{}, 3 * time.Second},
		{"while polling for a final scrape that never comes", func(*site) {}, summarize, 0, 4 * time.Second, history.Timeout{}, 4 * time.Second},
		{"the store status", func(s *site) { s.store.SetDelay(time.Minute, 0) }, func(ctx context.Context, s *site, until time.Time) error {
			_, err := s.client.StoreStatus(ctx, until)
			return err
		}, 0, 2 * time.Second, history.Timeout{}, 2 * time.Second},
		{"the preflight", func(s *site) { s.store.SetDelay(time.Minute, 0) }, func(ctx context.Context, s *site, until time.Time) error {
			return s.client.Preflight(ctx, until)
		}, 0, 2 * time.Second, history.Timeout{}, 2 * time.Second},
		// The first store call passes at once, the second waits 0.5 s for the
		// gate's spacing. The deadline falls inside that wait.
		{"a gate wait past the deadline starts no call", func(*site) {}, summarize, 0, 250 * time.Millisecond, history.Timeout{}, 250 * time.Millisecond},
	} {
		bubble(t, func(t *testing.T) {
			s := newSite(t, DefaultOptions, Config{})
			c.setup(s)
			ctx, cancel := context.WithCancel(t.Context())
			defer cancel()
			if c.after > 0 {
				time.AfterFunc(c.after, cancel)
			}
			started := time.Now()
			err := c.call(ctx, s, started.Add(c.until))
			eq(t, []any{err, time.Since(started)}, []any{c.want, c.took}, c.name)
			if c.until == 250*time.Millisecond {
				eq(t, len(s.store.Queries()), 1, "no call after the deadline")
			}
		})
	}
}

func summarize(ctx context.Context, s *site, until time.Time) error {
	_, err := s.client.Summarize(ctx, until, ready, params(-60, 0, 1))
	return err
}

// A store that answers at once runs no store call after a cancel.
func TestNoStoreCallAfterACancel(t *testing.T) {
	bubble(t, func(t *testing.T) {
		s := newSite(t, DefaultOptions, Config{})
		ctx, cancel := context.WithCancel(t.Context())
		s.store.Hook = func(fakeQuery) { cancel() }
		_, err := s.client.Summarize(ctx, time.Now().Add(time.Hour), ready, params(-7_200, -3_600, 1))
		eq(t, []any{err, len(s.store.Queries())}, []any{error(history.Cancelled{}), 1})
	})
}

// The host Ready-and-allocatable gate reads one flag series, the allocatable
// predicate: a sweep counts only where it is 1; missing or false is unavailable.
func TestTheAllocatableCapabilityIsReadAndEnforced(t *testing.T) {
	const (
		hostsGauge = probeapi.Prefix + "hosts"
		known      = probeapi.Prefix + "host_predicates_known"
	)
	gate := probeapi.Filter{Type: probeapi.UnitTypeHost, Stage: new(probeapi.StageReady), Allocatable: new(true)}
	for _, flag := range []float64{1, 0, math.NaN()} {
		bubble(t, func(t *testing.T) {
			s := newSite(t, DefaultOptions, Config{})
			// A zero baseline before the window, then one allocatable host,
			// each sweep with its allocatable flag unless NaN.
			for _, x := range []struct{ at, count, end float64 }{{-70, 0, -75}, {-20, 1, -25}, {0, 1, -5}} {
				s.store.Add(hostsGauge, ms(x.at), x.count, append([]string{"state", "Ready", "allocatable", "true"}, selector...)...)
				s.store.Add(probeapi.MetricSweepComplete, ms(x.at), base+x.end, append([]string{"reader", "devices", "family", "host"}, selector...)...)
				s.store.Add(probeapi.MetricSweepStart, ms(x.at), base+x.end-3, append([]string{"reader", "devices", "family", "host"}, selector...)...)
				s.store.Add(probeapi.MetricProcessStart, ms(x.at), process, selector...)
				if !math.IsNaN(flag) {
					s.store.Add(known, ms(x.at), flag, append([]string{"predicate", "allocatable"}, selector...)...)
				}
			}
			r, err := s.summary(gate, params(-60, 0, 1))
			wanted := known + `{predicate="allocatable",job="j",namespace="ns"}`
			queried := false
			for _, q := range s.store.Queries() {
				queried = queried || strings.HasPrefix(q.PromQL, wanted)
			}
			if flag == 1 {
				eq(t, []any{err, opt(r.FinalCount), reached(r), queried}, []any{nil, "1", at(-25), true}, flag)
			} else {
				eq(t, []any{errors.As(err, new(history.Unavailable)), queried}, []any{true, true}, flag, err)
			}
		})
	}
}
