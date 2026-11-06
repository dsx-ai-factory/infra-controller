// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"math"
	"net/http"
	"net/url"
	"slices"
	"strconv"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/history"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

const (
	// sampleBudget is the store samples one summary may hold, across every
	// reply, repeats included, so a long window cannot grow without bound. A
	// 24h window at a 5 s scrape is 17,280 samples per series: about 57 series.
	sampleBudget = 1_000_000
	// busyWait is how long a second summary waits for the one in flight,
	// or until its deadline if that is sooner. busyRetryAfter is when a busy
	// caller should ask again.
	busyWait       = 30 * time.Second
	busyRetryAfter = 10 * time.Second
	// recent is the floor for how far back the store status and the preflight
	// read for a live sample; the window scales up with the scrape interval.
	recent = 2 * time.Minute
)

// Options are how long a summary may read and take.
type Options struct {
	MaxWindow, SummaryTimeout time.Duration
}

// DefaultOptions read a day within 90 s.
var DefaultOptions = Options{MaxWindow: 24 * time.Hour, SummaryTimeout: 90 * time.Second}

// Status is how fresh the probe's series in the metrics store are.
type Status struct {
	LastSampleAt     *jsonx.Time
	SampleAgeSeconds *float64
	SweepCompleteAt  *jsonx.Time
}

// Client reads the probe's history from a metrics store for one run, with the
// probe metadata the run started with. One summary is in flight at a time; the
// caller's ctx and until bound every gate wait, retry, store call and poll.
type Client struct {
	meta     history.ProbeMetadata
	opts     Options
	queryURL *url.URL
	headers  http.Header
	token    *tokenFile
	delay    time.Duration
	http     *http.Client
	gate     *gate
	permit   chan struct{}
	budget   int
}

// New is a client of the store cfg describes. Its files are read once here, so a
// store that can never be queried is refused now. The summary timeout must leave
// room for the final scrape, and the ingestion delay be under half of it.
func New(cfg Config, meta history.ProbeMetadata, o Options) (*Client, error) {
	if o.MaxWindow <= 0 || o.SummaryTimeout <= 0 {
		return nil, history.Unsupported{Text: "the longest window and the summary timeout must be positive"}
	}
	headers, t, err := cfg.transport()
	if err != nil {
		return nil, err
	}
	// The final scrape is queryable only after the ingestion delay.
	delay := time.Duration(cfg.IngestionDelaySeconds) * time.Second
	if delay >= o.SummaryTimeout/2 {
		return nil, history.Unsupported{Text: "the metrics store ingestion delay is not under half the summary timeout"}
	}
	c := &Client{meta: meta, opts: o, headers: headers, delay: delay, gate: newGate(), permit: make(chan struct{}, 1),
		budget: sampleBudget,
		// Redirects are refused: a store has no reason to send its client
		// elsewhere, and the query URL can carry credentials.
		http: &http.Client{Transport: t, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }},
	}
	if cfg.BearerTokenFile != nil {
		path := *cfg.BearerTokenFile
		token, err := bearer(path)
		if err != nil {
			return nil, err
		}
		c.token = &tokenFile{timeout: callTimeout, lock: make(chan struct{}, 1), token: token, readAt: time.Now(),
			read: func() tokenRead { tok, err := bearer(path); return tokenRead{tok, err} }}
	}
	u, err := url.Parse(cfg.URL)
	// Secrets come only from mounted files, so a URL with credentials, a query or
	// a fragment is refused, not stripped and sent: no userinfo becomes Basic
	// auth. The host must be present and the port, if any, in range.
	if err != nil || u.Opaque != "" || (u.Scheme != "http" && u.Scheme != "https") ||
		u.User != nil || u.RawQuery != "" || u.ForceQuery || u.Fragment != "" || u.Hostname() == "" {
		return nil, unsupported("URL is not an http(s) URL")
	}
	if p := u.Port(); p != "" {
		if n, perr := strconv.Atoi(p); perr != nil || n < 1 || n > 65535 {
			return nil, unsupported("URL is not an http(s) URL")
		}
	}
	c.queryURL = u.JoinPath("api", "v1", "query")
	return c, nil
}

// errDeadline is the cause a bounded call's context is cancelled with when
// its until passes, so the deadline is told apart from a caller's cancel.
var errDeadline = errors.New("store: summary deadline")

// bounded runs call under a context that ctx or the time until ends, whichever
// comes first. It checks before and after: a cancel of ctx is history.Cancelled,
// the deadline history.Timeout, cancel first; a reply past the end is not taken.
func bounded[T any](ctx context.Context, until time.Time, call func(context.Context) (T, error)) (T, error) {
	var zero T
	// The child wait ends at the earlier of until and the caller's deadline, so
	// a nearer caller deadline bounds every wait even when its Done is late.
	if d, ok := ctx.Deadline(); ok && d.Before(until) {
		until = d
	}
	if err := boundErr(ctx, until); err != nil {
		return zero, err
	}
	child, cancel := context.WithCancelCause(ctx)
	defer cancel(nil)
	defer time.AfterFunc(time.Until(until), func() { cancel(errDeadline) }).Stop()
	v, err := call(child)
	if cerr := boundErr(ctx, until); cerr != nil {
		return zero, cerr
	}
	return v, err
}

// ctxErr is history.Cancelled for an explicit cancel and history.Timeout for an
// elapsed deadline, including one whose Done has not fired yet. Cancel wins a
// tie; nil while the context is live.
func ctxErr(ctx context.Context) error {
	switch err := ctx.Err(); {
	case errors.Is(err, context.Canceled):
		return history.Cancelled{}
	case err != nil:
		return history.Timeout{} // context.DeadlineExceeded
	}
	if d, ok := ctx.Deadline(); ok && !time.Now().Before(d) {
		return history.Timeout{}
	}
	return nil
}

// boundErr is the context's error, else history.Timeout once until has passed.
// Cancel is checked first.
func boundErr(ctx context.Context, until time.Time) error {
	if err := ctxErr(ctx); err != nil {
		return err
	}
	if !time.Now().Before(until) {
		return history.Timeout{}
	}
	return nil
}

// end is the end of a call started now with the caller's until.
func (c *Client) end(until time.Time) time.Time {
	if end := time.Now().Add(c.opts.SummaryTimeout); end.Before(until) {
		return end
	}
	return until
}

func (c *Client) selector(m ...[3]string) [][3]string {
	for _, k := range slices.Sorted(maps.Keys(c.meta.Info.Selector)) {
		m = append(m, [3]string{k, "=", c.meta.Info.Selector[k]})
	}
	return m
}

func msSecs(ms int64) float64 { return float64(ms) / 1000 }

// Preflight checks the store and the probe before a run: every required family
// is read, and the newest process start of every series scraped since the
// probe's started is that process. Earlier processes and pods may differ.
func (c *Client) Preflight(ctx context.Context, until time.Time, required ...probeapi.Family) error {
	for _, f := range required {
		if _, ok := c.meta.ReaderIntervals[f]; !ok {
			return history.Unsupported{Text: "the probe reads no " + string(f) + " sweeps"}
		}
	}
	w := c.recentWindow()
	q := query{names: []string{probeapi.MetricProcessStart}, matchers: c.selector(), span: w}
	series, err := bounded(ctx, c.end(until), func(ctx context.Context) ([]history.RawSeries, error) {
		return c.raw(ctx, q, time.Now())
	})
	if err != nil {
		return err
	}
	// The engine binds evidence to the run's process by exact float bits, so
	// preflight uses the same identity: a store that rounds the process start
	// is refused here, not accepted within a tolerance the engine would reject.
	process, pbits, live := c.meta.Info.ProcessStartSeconds, math.Float64bits(c.meta.Info.ProcessStartSeconds), false
	for _, s := range series {
		if len(s.Samples) == 0 {
			continue
		}
		newest := slices.MaxFunc(s.Samples, func(a, b history.Sample) int { return int(a.AtMs - b.AtMs) })
		if msSecs(newest.AtMs) < process {
			continue
		}
		if math.Float64bits(newest.Value) != pbits {
			return history.Unavailable{Text: "the selected series are not the probe process that served the metadata"}
		}
		live = true
	}
	if !live {
		return history.Unavailable{Text: fmt.Sprintf("the store has no sample of the live probe in the last %v", w)}
	}
	return nil
}

// recentWindow is how far back the preflight and store status read for a live
// sample: it scales with the scrape interval and delay (a slow store's newest
// sample is older), floored by recent and capped by the max summary window.
// A huge accepted interval is saturated before the arithmetic, which would
// otherwise overflow a duration and wrap back to the floor.
func (c *Client) recentWindow() time.Duration {
	limit := c.opts.MaxWindow
	if c.delay >= limit || c.meta.ScrapeInterval > (limit-c.delay)/2 {
		return limit
	}
	return min(max(2*c.meta.ScrapeInterval+c.delay, recent), limit)
}

// StoreStatus is the newest host sweep end sample in the store, and how old
// it is.
func (c *Client) StoreStatus(ctx context.Context, until time.Time) (Status, error) {
	var status Status
	q := query{names: []string{probeapi.MetricSweepComplete}, matchers: c.selector([3]string{"family", "=", string(probeapi.FamilyHost)}), span: c.recentWindow()}
	now := time.Now()
	series, err := bounded(ctx, c.end(until), func(ctx context.Context) ([]history.RawSeries, error) {
		return c.raw(ctx, q, now)
	})
	var last *history.Sample
	for _, s := range series {
		for i := range s.Samples {
			if last == nil || s.Samples[i].AtMs > last.AtMs {
				last = &s.Samples[i]
			}
		}
	}
	switch {
	case err != nil || last == nil:
		return status, err
	case !history.IsTime(last.Value):
		return status, history.Unavailable{Text: "the stored series are not the probe's: a sweep or process time is out of range"}
	}
	nowSecs := float64(now.Unix()) + float64(now.Nanosecond())/1e9
	status.LastSampleAt = new(history.AtSecs(msSecs(last.AtMs)))
	status.SampleAgeSeconds = new(max(nowSecs-msSecs(last.AtMs), 0))
	status.SweepCompleteAt = new(history.AtSecs(last.Value))
	return status, nil
}

// Summarize is the summary of f over p, within until and the summary
// timeout. A second summary waits for the one in flight: a wait that ends
// first is history.Busy.
func (c *Client) Summarize(ctx context.Context, until time.Time, f probeapi.Filter, p probeapi.SummaryParams) (probeapi.SummaryResponse, error) {
	var none probeapi.SummaryResponse
	until = c.end(until)
	hc := history.Context{Probe: c.meta, Now: time.Now, Until: until}
	r, err := history.Prepare(ctx, hc, f, p, c.opts.MaxWindow)
	if err != nil {
		return none, err
	}
	interval, ok := c.meta.ReaderIntervals[r.Spec.Family]
	if !ok {
		return none, history.Unsupported{Text: "the probe reads no " + string(r.Spec.Family) + " sweeps"}
	}
	// One summary in flight. The next waits, within its deadline. A wait the
	// deadline cuts short is busy too, never a store timeout.
	busyUntil := time.Now().Add(busyWait)
	if until.Before(busyUntil) {
		busyUntil = until
	}
	// Wake at the caller's deadline too, so a deadline whose Done is delayed
	// ends the wait as a timeout, not as busy.
	wake := busyUntil
	if d, ok := ctx.Deadline(); ok && d.Before(wake) {
		wake = d
	}
	timer := time.NewTimer(time.Until(wake))
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return none, ctxErr(ctx)
	case <-timer.C:
		if err := ctxErr(ctx); err != nil {
			return none, err
		}
		return none, history.Busy{RetryAfter: busyRetryAfter}
	case c.permit <- struct{}{}:
		defer func() { <-c.permit }()
	}
	if err := ctxErr(ctx); err != nil {
		return none, err
	}
	if !time.Now().Before(busyUntil) {
		return none, history.Busy{RetryAfter: busyRetryAfter}
	}
	return bounded(ctx, until, func(ctx context.Context) (probeapi.SummaryResponse, error) {
		return c.read(ctx, hc, r, interval)
	})
}

// gathered is the samples read so far, merged by series: index 0 the sweep
// times, 1 the counts, then one per need. count is every sample the store
// sent, repeats included.
type gathered struct {
	series []map[string]*history.RawSeries
	at     []map[string]map[int64]float64
	count  int
}

// merge folds reply into the gathered series. A sample equal to one already held,
// from a re-read or an overlapping scraper, is kept once. Two different values
// for one series and time are history.Unavailable, including across polls.
func (g *gathered) merge(i int, reply []history.RawSeries) error {
	for _, s := range reply {
		key, _ := json.Marshal(s.Labels)
		if g.series[i][string(key)] == nil {
			g.series[i][string(key)], g.at[i][string(key)] = &history.RawSeries{Labels: s.Labels}, map[int64]float64{}
		}
		for _, x := range s.Samples {
			if prev, ok := g.at[i][string(key)][x.AtMs]; ok && prev != x.Value {
				return unavailable("the store returned two values for one sample")
			}
			g.at[i][string(key)][x.AtMs] = x.Value
		}
	}
	return nil
}

// samples is what was read, keeping samples at or after fromMs.
func (g *gathered) samples(fromMs int64) history.Samples {
	all := make([][]history.RawSeries, len(g.series))
	for i := range g.series {
		for key, s := range g.series[i] {
			out := history.RawSeries{Labels: s.Labels}
			for _, at := range slices.Sorted(maps.Keys(g.at[i][key])) {
				if at >= fromMs {
					out.Samples = append(out.Samples, history.Sample{AtMs: at, Value: g.at[i][key][at]})
				}
			}
			if len(out.Samples) > 0 {
				all[i] = append(all[i], out)
			}
		}
	}
	return history.Samples{Times: all[0], Counts: all[1], Caps: all[2:]}
}

// read reads the window from the store and summarizes it.
func (c *Client) read(ctx context.Context, hc history.Context, r *history.Request, interval time.Duration) (probeapi.SummaryResponse, error) {
	queries := []query{
		// The empty alternative picks the process start, which has no family
		// label.
		{names: []string{probeapi.MetricSweepStart, probeapi.MetricSweepComplete, probeapi.MetricProcessStart},
			matchers: c.selector([3]string{"family", "=~", string(r.Spec.Family) + "|"})},
		{names: []string{probeapi.Prefix + r.Spec.Metric}},
	}
	for _, m := range r.Spec.Matchers {
		queries[1].matchers = append(queries[1].matchers, [3]string{m[0], "=", m[1]})
	}
	queries[1].matchers = c.selector(queries[1].matchers...)
	// One flag series per need: the gauge whose label says the sweep met it.
	for _, need := range r.Spec.Needs {
		capability := need.Capability()
		queries = append(queries, query{names: []string{capability.Gauge}, matchers: c.selector([3]string{capability.Label, "=", capability.Value})})
	}
	g := gathered{series: make([]map[string]*history.RawSeries, len(queries)), at: make([]map[string]map[int64]float64, len(queries))}
	for i := range queries {
		g.series[i], g.at[i] = map[string]*history.RawSeries{}, map[string]map[int64]float64{}
	}
	// fetch reads (upper - span, upper] for every query in one range query, no
	// chunking. Sweep times come first: a scrape lands whole, so one whose times
	// this read sees is whole by the count and flag reads after it. Every reply's
	// samples count against the cumulative budget.
	fetch := func(from, upper time.Time) error {
		span := upper.Sub(from)
		if span <= 0 {
			return nil
		}
		for i, q := range queries {
			q.span = span
			reply, err := c.raw(ctx, q, upper)
			if err != nil {
				return err
			}
			for _, s := range reply {
				g.count += len(s.Samples)
			}
			if g.count > c.budget {
				return history.InvalidInput{Field: "start", Reason: fmt.Sprintf(
					"the window and filter select more than %d store samples: shorten the window or narrow the filter", c.budget)}
			}
			if err := g.merge(i, reply); err != nil {
				return err
			}
		}
		return nil
	}
	// The window plus two reader intervals before it. A store with an
	// ingestion delay may hold only part of a newer scrape, so every read
	// stops that far before now. A PromQL range selector excludes its lower
	// bound, so the read starts one millisecond earlier to keep a baseline
	// ending exactly two reader intervals before the start, which the
	// fleet-time math accepts.
	epoch := time.Unix(0, 0)
	from := r.Start.Add(-2 * interval).Add(-time.Millisecond)
	if from.Before(epoch) {
		from = epoch
	}
	landed := func(limit time.Time) time.Time {
		if now := time.Now().Add(-c.delay); now.Before(limit) {
			return now
		}
		return limit
	}
	if err := fetch(from, landed(r.End)); err != nil {
		return probeapi.SummaryResponse{}, err
	}
	// Wait for a whole scrape at or after the end. It lands within three scrape
	// intervals of the end if at all, so only that span is read, never the gap to
	// today, and each poll re-reads it up to where scrapes have landed. The polls
	// start one interval before the end and run until settled: another scraper's
	// later scrape can land first, so one seen earlier does not end them.
	scrape := c.meta.ScrapeInterval
	horizon, settled := r.End.Add(3*scrape), r.End.Add(scrape)
	polled := r.End.Add(-scrape - c.delay)
	if polled.Before(epoch) {
		polled = epoch
	}
	for {
		upper := landed(horizon)
		if err := fetch(polled, upper); err != nil {
			return probeapi.SummaryResponse{}, err
		}
		seen, err := history.FinalScrapeSeen(ctx, hc, r, g.samples(r.End.UnixMilli()))
		if err != nil {
			return probeapi.SummaryResponse{}, err
		}
		if seen && !upper.Before(settled) || !upper.Before(horizon) {
			break
		}
		if err := sleep(ctx, max(scrape/2, 100*time.Millisecond)); err != nil {
			return probeapi.SummaryResponse{}, err
		}
	}
	return history.Summarize(ctx, hc, r, g.samples(math.MinInt64))
}
