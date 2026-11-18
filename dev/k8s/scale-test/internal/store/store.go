// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package store reads the probe's history from a Prometheus-compatible store
// with the instant query API: a range selector returns the raw samples, so the
// summary sees every scrape, not a resampled curve. The store must make each
// scrape queryable whole, or declare how long that takes (IngestionDelaySeconds).
package store

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/history"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

const (
	// ReplyCap is the biggest store reply. One hour at a 5 s scrape is 720
	// samples, about 17 KiB of JSON per series, so this holds about 900 series.
	ReplyCap = 16 << 20
	// callsPerSecond is the most store calls started per second.
	callsPerSecond = 2
	// callTimeout bounds one store call, its reply body included.
	callTimeout = 5 * time.Second
	// maxRetries is the retries of one store call after its first attempt.
	maxRetries = 2
	// firstBackoff is the first pushback wait. Later ones double it, up to
	// maxBackoff, a store's Retry-After included.
	firstBackoff = time.Second
	maxBackoff   = 30 * time.Second
)

// query is a range selector over probe series: more than one name is a
// __name__ regex. A matcher is a label, = or =~, and a value.
type query struct {
	names    []string
	matchers [][3]string
	span     time.Duration
}

// promql is the PromQL text of q. Its range covers span exactly, or
// outward: whole seconds as Ns, anything else in milliseconds, rounded up.
func (q query) promql() string {
	var matchers []string
	text := ""
	if len(q.names) == 1 {
		text = q.names[0]
	} else {
		prefix := probeapi.Prefix
		for _, n := range q.names {
			if !strings.HasPrefix(n, prefix) {
				prefix = ""
			}
		}
		alternatives := make([]string, len(q.names))
		for i, n := range q.names {
			alternatives[i] = strings.TrimPrefix(n, prefix)
		}
		matchers = append(matchers, `__name__=~"`+prefix+"("+strings.Join(alternatives, "|")+`)"`)
	}
	for _, m := range q.matchers {
		// A JSON string is a PromQL string: PromQL takes Go escapes.
		var quoted bytes.Buffer
		enc := json.NewEncoder(&quoted)
		enc.SetEscapeHTML(false)
		_ = enc.Encode(m[2])
		matchers = append(matchers, m[0]+m[1]+strings.TrimSuffix(quoted.String(), "\n"))
	}
	ms := max(1, (q.span+time.Millisecond-1)/time.Millisecond)
	span := fmt.Sprintf("%dms", ms)
	if ms%1000 == 0 {
		span = fmt.Sprintf("%ds", ms/1000)
	}
	return text + "{" + strings.Join(matchers, ",") + "}[" + span + "]"
}

func unavailable(what string) error { return history.Unavailable{Text: "metrics store: " + what} }

// sample is one [time, "value"] pair of a matrix reply.
type sample struct {
	at    float64
	value string
}

// errBadSample is a matrix tuple that is not exactly a [number, "value"] pair:
// a null part, a missing part, or extra elements. Its text holds no reply.
var errBadSample = errors.New("a sample is not a [time, value] pair")

func (s *sample) UnmarshalJSON(b []byte) error {
	var raw []json.RawMessage
	if err := json.Unmarshal(b, &raw); err != nil {
		return err
	}
	if len(raw) != 2 || string(raw[0]) == "null" || string(raw[1]) == "null" {
		return errBadSample
	}
	if err := json.Unmarshal(raw[0], &s.at); err != nil {
		return err
	}
	return json.Unmarshal(raw[1], &s.value)
}

// partialReply is a reply a cluster store answered without some of its
// storage nodes: never taken.
var partialReply = unavailable("partial reply")

// distinctKeys reports whether every object in body, at any depth, has
// distinct keys. The reply envelope decodes into structs, which encoding/json
// matches case-insensitively and keeps the last, so a cased or repeated
// envelope key, even a nested RESULT, could overwrite data and is refused. The
// metric object is a case-sensitive string map, so only an exactly repeated
// label collides there; labels such as job and JOB both survive.
func distinctKeys(body []byte) bool {
	// json.Valid bounds nesting with the scanner's iterative limit; running it
	// first keeps the recursive walk below that bound, so a deeply nested value
	// cannot exhaust the stack here. An invalid or over-nested body is refused.
	if !json.Valid(body) {
		return false
	}
	collision, _ := distinctValue(json.NewDecoder(bytes.NewReader(body)), true)
	return !collision
}

// distinctValue reports a key collision in the next value or any value nested
// in it, and the decode error that stopped the walk. When fold is set an
// object's keys collide under case folding, as its struct fields do; the value
// of a metric key is a case-sensitive label map, so it and anything under it
// collide only on an exact repeat. On an error the walk stops at once, so a
// malformed stream cannot spin More.
func distinctValue(dec *json.Decoder, fold bool) (collision bool, err error) {
	tok, err := dec.Token()
	if err != nil {
		return false, err
	}
	switch delim, isDelim := tok.(json.Delim); {
	case isDelim && delim == '{':
		seen := map[string]bool{}
		for dec.More() {
			key, err := dec.Token()
			if err != nil {
				return false, err
			}
			name := key.(string)
			k := name
			if fold {
				k = strings.ToLower(name)
			}
			if seen[k] {
				return true, nil
			}
			seen[k] = true
			// encoding/json decodes the metric key, in any case, into a string
			// map, so its labels match exactly, not folded.
			if c, err := distinctValue(dec, fold && !strings.EqualFold(name, "metric")); c || err != nil {
				return c, err
			}
		}
		_, err = dec.Token() // the closing brace
	case isDelim && delim == '[':
		for dec.More() {
			if c, err := distinctValue(dec, fold); c || err != nil {
				return c, err
			}
		}
		_, err = dec.Token() // the closing bracket
	}
	return false, err
}

// parseMatrix is the matrix of an instant query reply. Errors are fixed
// text: nothing of the reply reaches them.
func parseMatrix(body []byte) ([]history.RawSeries, error) {
	var reply struct {
		Status string `json:"status"`
		// A VictoriaMetrics cluster sets isPartial when a storage node did not
		// answer. Thanos partial responses and failed Prometheus remote reads warn.
		IsPartial bool              `json:"isPartial"`
		Warnings  []json.RawMessage `json:"warnings"`
		Data      *struct {
			ResultType string `json:"resultType"`
			// A pointer tells a missing result from an empty one (a bare success is
			// malformed); Values is raw to tell an explicit null from a missing field.
			Result *[]struct {
				Metric map[string]string `json:"metric"`
				Values json.RawMessage   `json:"values"`
			} `json:"result"`
		} `json:"data"`
	}
	switch {
	case !utf8.Valid(body) || !distinctKeys(body) || json.Unmarshal(body, &reply) != nil:
		return nil, unavailable("undecodable reply")
	case reply.Status != "success":
		return nil, unavailable("query failed")
	case reply.IsPartial || len(reply.Warnings) > 0:
		return nil, partialReply
	case reply.Data == nil:
		return nil, unavailable("reply has no data")
	case reply.Data.ResultType != "matrix":
		return nil, unavailable("reply is not a matrix")
	case reply.Data.Result == nil:
		return nil, unavailable("reply has no result")
	}
	out := []history.RawSeries{}
	for _, s := range *reply.Data.Result {
		// A missing or null metric is refused. A missing values field is the empty
		// default; an explicit null is refused, never read as no samples.
		if s.Metric == nil {
			return nil, unavailable("a series has no metric")
		}
		var values []sample
		switch {
		case bytes.Equal(bytes.TrimSpace(s.Values), []byte("null")):
			return nil, unavailable("a series has a null values list")
		case len(s.Values) > 0 && json.Unmarshal(s.Values, &values) != nil:
			return nil, unavailable("undecodable reply")
		}
		// A _timestamp_seconds series holds Unix times: sweep start and end,
		// process start.
		times := strings.HasSuffix(s.Metric["__name__"], "_timestamp_seconds")
		series := history.RawSeries{Labels: s.Metric}
		for _, x := range values {
			v, err := strconv.ParseFloat(x.value, 64)
			switch {
			case !history.IsTime(x.at):
				return nil, unavailable("a sample time is out of range")
			case err != nil || math.IsInf(v, 0) || math.IsNaN(v) || strings.ContainsAny(x.value, "xX_"):
				return nil, unavailable("a sample is not a finite number")
			case times && !history.IsTime(v):
				return nil, unavailable("a time sample is out of range")
			}
			series.Samples = append(series.Samples, history.Sample{AtMs: int64(math.Round(x.at * 1000)), Value: v})
		}
		out = append(out, series)
	}
	return out, nil
}

// callError is why one store call failed, as a category safe to log and
// return: a status code at most, never reply text or a URL.
type callError struct {
	kind string
	// retryAfter is a pushback's Retry-After, 0 when none.
	retryAfter time.Duration
}

func (e callError) Error() string { return e.kind }

const (
	timedOut   = "timed out"
	pushedBack = "pushed back"
	transport  = "transport error"
	partial    = "partial reply"
)

// retryable reports whether a retry may pass: timeouts, pushback, transport
// errors and partial replies. An error answer or an undecodable reply will not.
func (e callError) retryable() bool {
	return e.kind == timedOut || e.kind == pushedBack || e.kind == transport || e.kind == partial
}

// classify maps a reply status: nil for a success. 429 and 503 are pushback with
// a Retry-After. 408 and 504 are a timeout; anything else is an error answer.
func classify(r *http.Response) error {
	switch s := r.StatusCode; {
	case s >= 200 && s < 300:
		return nil
	case s == http.StatusTooManyRequests || s == http.StatusServiceUnavailable:
		after, _ := strconv.ParseUint(strings.TrimSpace(r.Header.Get("Retry-After")), 10, 64)
		return callError{pushedBack, time.Duration(min(after, uint64(maxBackoff/time.Second))) * time.Second}
	case s == http.StatusGatewayTimeout || s == http.StatusRequestTimeout:
		return callError{kind: timedOut}
	default:
		return callError{kind: fmt.Sprintf("HTTP %d", s)}
	}
}

// gate serializes and spaces the calls to one store: one call in flight,
// callsPerSecond starts, each bounded by its timeout. Pushback widens the
// spacing from firstBackoff, doubling to maxBackoff, until a call succeeds.
type gate struct {
	spacing, timeout time.Duration
	// permit holds the call in flight. Blocked senders queue in order. It is
	// the only lock: nextStart and backoff are touched only by its holder, and
	// the channel orders one holder before the next.
	permit    chan struct{}
	nextStart time.Time
	backoff   time.Duration
}

func newGate() *gate {
	return &gate{spacing: time.Second / callsPerSecond, timeout: callTimeout, permit: make(chan struct{}, 1)}
}

func sleep(ctx context.Context, d time.Duration) error {
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-t.C:
		return nil
	}
}

// call runs f once at its turn, under the call timeout. f returns the moment
// its request and body read finished, which the call timeout bounds; work f
// does after that, such as parsing, is not timed.
func (g *gate) call(ctx context.Context, f func(context.Context) (time.Time, error)) error {
	select {
	case g.permit <- struct{}{}:
		defer func() { <-g.permit }()
	case <-ctx.Done():
		return ctx.Err()
	}
	if err := sleep(ctx, time.Until(g.nextStart)); err != nil {
		return err
	}
	started := time.Now()
	g.nextStart = started.Add(g.spacing + g.backoff)
	callCtx, cancel := context.WithTimeout(ctx, g.timeout)
	readAt, err := f(callCtx)
	cancel()
	// The timer wins ties: a request and body read that finished only past the
	// timeout is a timeout. A reply read in time but parsed slowly is not.
	if ctx.Err() == nil && readAt.Sub(started) >= g.timeout {
		err = callError{kind: timedOut}
	}
	var e callError
	switch {
	case err == nil:
		g.backoff, g.nextStart = 0, started.Add(g.spacing)
	case errors.As(err, &e) && e.kind == pushedBack:
		grown := firstBackoff
		if g.backoff > 0 {
			grown = min(2*g.backoff, maxBackoff)
		}
		// A store's hint may lengthen the wait, within the cap, never
		// shorten it. The back-off runs from the pushback reply.
		g.backoff = max(grown, min(e.retryAfter, maxBackoff))
		g.nextStart = started.Add(g.spacing)
		if after := time.Now().Add(g.backoff); after.After(g.nextStart) {
			g.nextStart = after
		}
	}
	return err
}

// do runs f through the gate, retrying at most maxRetries times while the
// error is retryable. Each attempt waits its turn.
func (g *gate) do(ctx context.Context, f func(context.Context) (time.Time, error)) error {
	for attempt := 0; ; attempt++ {
		err := g.call(ctx, f)
		var e callError
		if !errors.As(err, &e) || !e.retryable() || attempt == maxRetries || ctx.Err() != nil {
			return err
		}
	}
}

// raw is the raw samples of q in (at - span, at]. Errors are history.Unavailable
// with fixed text, history.Timeout, or history.Unsupported for an unreadable token.
func (c *Client) raw(ctx context.Context, q query, at time.Time) ([]history.RawSeries, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	u := *c.queryURL
	u.RawQuery = url.Values{"query": {q.promql()}, "time": {fmt.Sprintf("%d.%03d", at.Unix(), at.Nanosecond()/1e6)}}.Encode()
	token := ""
	if c.token != nil {
		var err error
		if token, err = c.token.current(ctx); err != nil {
			return nil, err
		}
	}
	var series []history.RawSeries
	err := c.gate.do(ctx, func(ctx context.Context) (time.Time, error) {
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, u.String(), nil)
		if err != nil {
			return time.Now(), callError{kind: transport}
		}
		req.Header = c.headers.Clone()
		if token != "" {
			req.Header.Set("Authorization", token)
		}
		resp, err := c.http.Do(req)
		if err != nil {
			// The error drops the URL: it carries the query and can carry
			// credentials.
			var netErr net.Error
			if errors.Is(err, context.DeadlineExceeded) || errors.As(err, &netErr) && netErr.Timeout() {
				return time.Now(), callError{kind: timedOut}
			}
			return time.Now(), callError{kind: transport}
		}
		defer func() { _ = resp.Body.Close() }()
		// A refusal is its status code, whatever its body says. The body is
		// never read.
		if err := classify(resp); err != nil {
			return time.Now(), err
		}
		body, err := io.ReadAll(io.LimitReader(resp.Body, ReplyCap+1))
		switch {
		case err != nil:
			return time.Now(), callError{kind: transport}
		case len(body) > ReplyCap:
			return time.Now(), callError{kind: "undecodable reply"}
		}
		// The request and body read are done; the call timeout covered them.
		// parseMatrix below is not timed, so a slow parse is not a timeout.
		read := time.Now()
		// A partial reply may be whole on the next try. Any other reply is
		// final.
		if series, err = parseMatrix(body); err == partialReply {
			return read, callError{kind: partial}
		}
		return read, err
	})
	var e callError
	switch {
	case !errors.As(err, &e):
		return series, err
	case e.kind == timedOut || e.kind == pushedBack:
		// Pushback that outlasts the retries is a store that did not answer
		// in time.
		return nil, history.Timeout{}
	default:
		return nil, unavailable(e.kind)
	}
}
