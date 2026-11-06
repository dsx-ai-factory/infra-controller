// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"bufio"
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"encoding/pem"
	"fmt"
	"math/big"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"testing"
	"testing/synctest"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/history"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

func eq(t *testing.T, got, want any, what ...any) {
	t.Helper()
	if !reflect.DeepEqual(got, want) {
		t.Errorf("%v: got %v, want %v", what, got, want)
	}
}

func unavailableText(what string) error { return history.Unavailable{Text: "metrics store: " + what} }

const sentinel = "SENTINEL-c0ffee"

var units = query{names: []string{probeapi.Prefix + "units"}, span: time.Minute}

func TestPromQL(t *testing.T) {
	selector := [][3]string{{"job", "=", "nico-site-health-probe-metrics"}, {"namespace", "=", "nico-system"}}
	for _, c := range []struct {
		q    query
		want string
	}{
		{query{[]string{"carbide_site_health_probe_units"}, append([][3]string{{"type", "=", "host"}, {"stage", "=", "ready"}}, selector...), time.Hour},
			`carbide_site_health_probe_units{type="host",stage="ready",job="nico-site-health-probe-metrics",namespace="nico-system"}[3600s]`},
		{query{[]string{probeapi.MetricSweepStart, probeapi.MetricSweepComplete, probeapi.MetricSweepsTotal, probeapi.MetricProcessStart},
			append([][3]string{{"family", "=~", "host|"}}, selector...), 900 * time.Second},
			`{__name__=~"carbide_site_health_probe_(sweep_start_timestamp_seconds|sweep_complete_timestamp_seconds|sweeps_total|process_start_timestamp_seconds)",family=~"host|",job="nico-site-health-probe-metrics",namespace="nico-system"}[900s]`},
		{query{[]string{"x_a", "y_b"}, nil, time.Second}, `{__name__=~"(x_a|y_b)"}[1s]`},
		{query{[]string{"carbide_site_health_probe_units"}, nil, 2500 * time.Millisecond}, "carbide_site_health_probe_units{}[2500ms]"},
		{query{[]string{"carbide_site_health_probe_units"}, nil, 2_000_001 * time.Microsecond}, "carbide_site_health_probe_units{}[2001ms]"},
		{query{[]string{"carbide_site_health_probe_hosts"}, [][3]string{{"model", "=", `a"b\c<&>`}}, time.Minute}, `carbide_site_health_probe_hosts{model="a\"b\\c<&>"}[60s]`},
		{query{[]string{"carbide_site_health_probe_hosts"}, [][3]string{{"model", "=", "a\nb\rc\td"}}, time.Minute}, `carbide_site_health_probe_hosts{model="a\nb\rc\td"}[60s]`},
	} {
		eq(t, c.q.promql(), c.want)
	}
}

func matrix(name, values string) string {
	return `{"status":"success","data":{"resultType":"matrix","result":[{"metric":{"__name__":"` + name + `","type":"host"},"values":` + values + `}]}}`
}

func TestParseMatrix(t *testing.T) {
	got, err := parseMatrix([]byte(matrix(probeapi.Prefix+"units", `[[1761213600.0,"12"],[1761213605.5,"13"],[1761213606,"1e3"]]`)))
	eq(t, []any{err, got[0].Labels["type"], got[0].Samples}, []any{nil, "host", []history.Sample{{AtMs: 1_761_213_600_000, Value: 12}, {AtMs: 1_761_213_605_500, Value: 13}, {AtMs: 1_761_213_606_000, Value: 1000}}})
	// A VictoriaMetrics reply: integer times, its stats, isPartial false.
	_, err = parseMatrix([]byte(`{"status":"success","isPartial":false,"data":{"resultType":"matrix","result":[]},"stats":{"seriesFetched":"1"},"warnings":null}`))
	eq(t, err, nil, "VictoriaMetrics")
	for _, c := range []struct{ body, want string }{
		{matrix(probeapi.Prefix+"units", `[[1761213600,"NaN"]]`), "a sample is not a finite number"},
		{matrix(probeapi.Prefix+"units", `[[1761213600,"+Inf"]]`), "a sample is not a finite number"},
		{matrix(probeapi.Prefix+"units", `[[1761213600,"-Inf"]]`), "a sample is not a finite number"},
		{matrix(probeapi.Prefix+"units", `[[1761213600,"twelve"]]`), "a sample is not a finite number"},
		{matrix(probeapi.Prefix+"units", `[[1761213600,"0x1p4"]]`), "a sample is not a finite number"},
		{matrix(probeapi.Prefix+"units", `[[1761213600,`+sentinel+`]]`), "undecodable reply"},
		{matrix(probeapi.Prefix+"units", `[[1761213600,12]]`), "undecodable reply"},
		// Strict tuples: a null part, extra items, a missing value and an underscore number are refused.
		{matrix(probeapi.Prefix+"units", `[[null,"1"]]`), "undecodable reply"},
		{matrix(probeapi.Prefix+"units", `[[1761213600,null]]`), "undecodable reply"},
		{matrix(probeapi.Prefix+"units", `[[1761213600,"1",2]]`), "undecodable reply"},
		{matrix(probeapi.Prefix+"units", `[[1761213600]]`), "undecodable reply"},
		{matrix(probeapi.Prefix+"units", `[[1761213600,"1_000"]]`), "a sample is not a finite number"},
		// A success with no result field is malformed, not an empty matrix.
		{`{"status":"success","data":{"resultType":"matrix"}}`, "reply has no result"},
		// A differently cased isPartial must not override the canonical one,
		// nor a nested RESULT the populated result.
		{`{"status":"success","isPartial":true,"ISPARTIAL":false,"data":{"resultType":"matrix","result":[]}}`, "undecodable reply"},
		{`{"status":"success","data":{"resultType":"matrix","result":[{"metric":{},"values":[[1,"1"]]}],"RESULT":[]}}`, "undecodable reply"},
		// A null values list is refused; a null or missing metric is refused.
		{`{"status":"success","data":{"resultType":"matrix","result":[{"metric":{},"values":null}]}}`, "a series has a null values list"},
		{`{"status":"success","data":{"resultType":"matrix","result":[{"metric":null,"values":[]}]}}`, "a series has no metric"},
		{`{"status":"success","data":{"resultType":"matrix","result":[{"values":[]}]}}`, "a series has no metric"},
		{matrix(probeapi.Prefix+"units", `[[-1,"1"]]`), "a sample time is out of range"},
		{matrix(probeapi.Prefix+"units", `[[1e13,"1"]]`), "a sample time is out of range"},
		{matrix(probeapi.Prefix+"units", `[[1e308,"1"]]`), "a sample time is out of range"},
		{matrix(probeapi.MetricSweepComplete, `[[1761213600,"1e308"]]`), "a time sample is out of range"},
		{matrix(probeapi.MetricSweepStart, `[[1761213600,"-1"]]`), "a time sample is out of range"},
		{matrix(probeapi.MetricProcessStart, `[[1761213600,"1e13"]]`), "a time sample is out of range"},
		{`{"status":"success","data":{"resultType":"vector","result":[]}}`, "reply is not a matrix"},
		{`{"status":"success","data":{"resultType":"` + sentinel + `","result":[]}}`, "reply is not a matrix"},
		{`{"status":"success"}`, "reply has no data"},
		{`{"status":"error","error":"` + sentinel + `"}`, "query failed"},
		{`{"status":"success","isPartial":true,"data":{"resultType":"matrix","result":[]}}`, "partial reply"},
		{`{"status":"success","warnings":["` + sentinel + `"],"data":{"resultType":"matrix","result":[]}}`, "partial reply"},
		{sentinel, "undecodable reply"},
		{"{\"status\":\"\xff\"}", "undecodable reply"},
		// A value nested past the standard bound is refused before the walk
		// recurses into it.
		{strings.Repeat("[", 20_000) + strings.Repeat("]", 20_000), "undecodable reply"},
	} {
		_, err := parseMatrix([]byte(c.body))
		eq(t, err, unavailableText(c.want), c.body)
	}
	// A count is not a time.
	_, err = parseMatrix([]byte(matrix(probeapi.MetricSweepsTotal, `[[1761213600,"1e13"]]`)))
	eq(t, err, nil, "a count of 1e13")
	// A missing values field is the empty default, as the Rust type reads it.
	series, err := parseMatrix([]byte(`{"status":"success","data":{"resultType":"matrix","result":[{"metric":{"__name__":"x"}}]}}`))
	eq(t, []any{err, len(series), len(series[0].Samples)}, []any{nil, 1, 0}, "missing values")
}

// distinctKeys bounds its recursion: a value nested past the standard JSON
// depth limit is refused up front, so the walk cannot exhaust the stack.
func TestDistinctKeysRefusesOverDepth(t *testing.T) {
	deep := strings.Repeat("[", 20_000) + strings.Repeat("]", 20_000)
	eq(t, distinctKeys([]byte(deep)), false, "over the nesting bound")
}

// A metric object is a case-sensitive label map, so job and JOB are distinct
// and both survive, while an exactly repeated label is refused. Envelope fold
// collisions stay refused by TestParseMatrix.
func TestDistinctKeysFoldsEnvelopeKeysButNotMetricLabels(t *testing.T) {
	metric := func(body string) string {
		return `{"status":"success","data":{"resultType":"matrix","result":[{"metric":` + body + `,"values":[[1761213600,"1"]]}]}}`
	}
	series, err := parseMatrix([]byte(metric(`{"__name__":"carbide_site_health_probe_units","job":"a","JOB":"b"}`)))
	eq(t, []any{err, len(series) == 1}, []any{nil, true}, "job and JOB both survive")
	if len(series) == 1 {
		eq(t, []any{series[0].Labels["job"], series[0].Labels["JOB"]}, []any{"a", "b"}, "both labels kept")
	}
	for _, bad := range []string{
		metric(`{"job":"a","job":"b"}`),           // an exactly repeated label overwrites
		metric(`{"JOB":"a","job":"b","JOB":"c"}`), // a repeat among case variants
	} {
		_, err := parseMatrix([]byte(bad))
		eq(t, err, unavailableText("undecodable reply"), bad)
	}
}

// An hour of 900 host series at a 5 s scrape, with every label the probe
// and a scrape add, fits in one reply.
func TestReplyCapHoldsAnHourOfSeries(t *testing.T) {
	var b strings.Builder
	b.WriteString(`{"status":"success","data":{"resultType":"matrix","result":[`)
	for i := range 900 {
		if i > 0 {
			b.WriteByte(',')
		}
		fmt.Fprintf(&b, `{"metric":{"__name__":"carbide_site_health_probe_hosts","model":"PowerEdge R760xa model %d","instance_type":"instance-type-%d",`+
			`"state":"Assigned/Ready","allocatable":"true","update_complete":"false","job":"nico-site-health-probe-metrics","namespace":"nico-system",`+
			`"instance":"10.233.120.45:9009","pod":"nico-site-health-probe-7d9c8b6f5d-x2k4p","container":"site-health-probe","endpoint":"metrics",`+
			`"service":"nico-site-health-probe-metrics"},"values":[`, i%30, i/30)
		for k := range 720 {
			if k > 0 {
				b.WriteByte(',')
			}
			fmt.Fprintf(&b, `[%s,"4500"]`, strconv.FormatFloat(1_761_213_600.0+5*float64(k)+0.123, 'f', -1, 64))
		}
		b.WriteString("]}")
	}
	b.WriteString("]}}")
	series, err := parseMatrix([]byte(b.String()))
	eq(t, []any{b.Len() <= ReplyCap, len(series), err}, []any{true, 900, nil}, b.Len())
}

func TestClassify(t *testing.T) {
	for status, want := range map[int]error{
		200: nil, 429: callError{kind: pushedBack}, 503: callError{pushedBack, 7 * time.Second},
		504: callError{kind: timedOut}, 408: callError{kind: timedOut}, 302: callError{kind: "HTTP 302"},
	} {
		r := &http.Response{StatusCode: status, Header: http.Header{}}
		if status == 503 {
			r.Header.Set("Retry-After", " 7 ")
		}
		eq(t, classify(r), want, status)
	}
}

func TestGate(t *testing.T) {
	type attempt struct {
		reply error
		hold  time.Duration
	}
	// run calls f through a fresh gate, n times in turn, each attempt answering
	// the next reply. It returns each call's error, the gaps and the peak in-flight.
	run := func(n int, parallel bool, replies ...attempt) (errs []error, gaps []time.Duration, most int) {
		synctest.Test(t, func(t *testing.T) {
			g := newGate()
			var mu sync.Mutex
			var starts []time.Time
			inFlight := 0
			f := func(ctx context.Context) error {
				mu.Lock()
				starts, inFlight = append(starts, time.Now()), inFlight+1
				most = max(most, inFlight)
				a := attempt{}
				if len(replies) > 0 {
					a, replies = replies[0], replies[1:]
				}
				mu.Unlock()
				time.Sleep(a.hold)
				mu.Lock()
				inFlight--
				mu.Unlock()
				return a.reply
			}
			var wg sync.WaitGroup
			errs = make([]error, n)
			for i := range n {
				if !parallel {
					errs[i] = g.do(t.Context(), f)
					continue
				}
				wg.Go(func() { errs[i] = g.do(t.Context(), f) })
				time.Sleep(time.Millisecond)
			}
			wg.Wait()
			for i := 1; i < len(starts); i++ {
				gaps = append(gaps, starts[i].Sub(starts[i-1]))
			}
		})
		return errs, gaps, most
	}
	// One call in flight, starts at least 0.5 s apart.
	_, gaps, most := run(6, true, attempt{hold: 100 * time.Millisecond}, attempt{hold: 100 * time.Millisecond}, attempt{hold: 100 * time.Millisecond})
	eq(t, most, 1)
	for _, gap := range gaps {
		if gap < 500*time.Millisecond {
			t.Errorf("gaps %v", gaps)
		}
	}
	// Timeout, pushback, transport and partial replies are retried twice,
	// then final. An error answer or an undecodable reply is final at once.
	for _, e := range []callError{{kind: timedOut}, {kind: pushedBack}, {kind: transport}, {kind: partial}, {kind: "HTTP 400"}, {kind: "undecodable reply"}} {
		errs, gaps, _ := run(1, false, attempt{reply: e}, attempt{reply: e}, attempt{reply: e}, attempt{reply: e})
		eq(t, []any{errs[0], len(gaps) + 1}, []any{error(e), map[bool]int{true: 1 + maxRetries, false: 1}[e.retryable()]}, e)
	}
	errs, _, _ := run(1, false, attempt{reply: callError{kind: transport}})
	eq(t, errs, []error{nil}, "a retry that passes")
	// Pushback waits 1 s, then 2 s, and a success resets to 0.5 s.
	push := attempt{reply: callError{kind: pushedBack}}
	_, gaps, _ = run(2, false, push, push)
	eq(t, gaps, []time.Duration{time.Second, 2 * time.Second, 500 * time.Millisecond}, "back-off")
	// A Retry-After lengthens the wait, capped at 30 s. A shorter one never
	// shortens it.
	_, gaps, _ = run(1, false, attempt{reply: callError{pushedBack, 600 * time.Second}})
	eq(t, gaps, []time.Duration{maxBackoff}, "Retry-After")
	_, gaps, _ = run(1, false, push, push)
	eq(t, gaps, []time.Duration{time.Second, 2 * time.Second}, "1 s, then 2 s")
	// A call that runs past its timeout is a timeout, three times over, and
	// a reply ready only past the timeout is one too.
	synctest.Test(t, func(t *testing.T) {
		g := newGate()
		started := time.Now()
		err := g.do(t.Context(), func(ctx context.Context) error { <-ctx.Done(); return ctx.Err() })
		eq(t, []any{err, time.Since(started)}, []any{error(callError{kind: timedOut}), 3 * callTimeout}, "slow calls")
		err = g.call(t.Context(), func(context.Context) error { time.Sleep(callTimeout); return nil })
		eq(t, err, error(callError{kind: timedOut}), "a reply at the timeout")
	})
}

// served is a client whose store answers request i, from 0, with reply, in
// process, and the count of requests it got.
func served(t *testing.T, cfg Config, reply func(i int32, w http.ResponseWriter, r *http.Request)) (*Client, *atomic.Int32) {
	if cfg.URL == "" {
		cfg.URL = "http://store/prom"
	}
	c, err := New(cfg, metadata(t), DefaultOptions)
	if err != nil {
		t.Fatal(err)
	}
	var requests atomic.Int32
	c.http.Transport = fakeTransport{Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/prom/api/v1/query" || r.URL.Query().Get("query") == "" || r.URL.Query().Get("time") == "" {
			http.Error(w, "wrong request", http.StatusTeapot)
			return
		}
		reply(requests.Add(1)-1, w, r)
	})}
	return c, &requests
}

func reply(status int, body string) func(int32, http.ResponseWriter, *http.Request) {
	return func(_ int32, w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(status)
		_, _ = w.Write([]byte(body))
	}
}

const empty = `{"status":"success","data":{"resultType":"matrix","result":[]}}`

func TestStoreReplies(t *testing.T) {
	partialBody := `{"status":"success","isPartial":true,"data":{"resultType":"matrix","result":[{"metric":{},"values":[[1761213600,"1"]]}]}}`
	thenEmpty := func(first func(int32, http.ResponseWriter, *http.Request)) func(int32, http.ResponseWriter, *http.Request) {
		return func(i int32, w http.ResponseWriter, r *http.Request) {
			if i == 0 {
				first(i, w, r)
			} else {
				reply(200, empty)(i, w, r)
			}
		}
	}
	for _, c := range []struct {
		name  string
		reply func(int32, http.ResponseWriter, *http.Request)
		want  error
		calls int32
	}{
		{"matrix", reply(200, empty), nil, 1},
		{"400 is its status, never its text", reply(400, `{"status":"error","errorType":"bad_data","error":"`+sentinel+`"}`), unavailableText("HTTP 400"), 1},
		{"500", reply(500, `{}`), unavailableText("HTTP 500"), 1},
		{"a refusal with a matrix body", reply(400, empty), unavailableText("HTTP 400"), 1},
		{"401 not JSON", reply(401, "<html>"+sentinel+"</html>"), unavailableText("HTTP 401"), 1},
		{"404 not JSON", reply(404, "<html>"+sentinel+"</html>"), unavailableText("HTTP 404"), 1},
		{"302 without Location", reply(302, "moved"), unavailableText("HTTP 302"), 1},
		{"a redirect is never followed", func(_ int32, w http.ResponseWriter, r *http.Request) {
			http.Redirect(w, r, "/prom/api/v1/query?query=x&time=1", http.StatusFound)
		}, unavailableText("HTTP 302"), 1},
		{"slow reply", func(_ int32, w http.ResponseWriter, r *http.Request) {
			select {
			case <-r.Context().Done():
			case <-time.After(10 * time.Second):
			}
		}, history.Timeout{}, 1 + maxRetries},
		{"pushback once", thenEmpty(reply(429, "")), nil, 2},
		{"pushback always", reply(503, ""), history.Timeout{}, 1 + maxRetries},
		{"504", reply(504, ""), history.Timeout{}, 1 + maxRetries},
		{"408", reply(408, ""), history.Timeout{}, 1 + maxRetries},
		{"one byte over the cap", reply(200, empty+strings.Repeat(" ", ReplyCap+1-len(empty))), unavailableText("undecodable reply"), 1},
		{"partial once", thenEmpty(reply(200, partialBody)), nil, 2},
		{"partial always", reply(200, partialBody), unavailableText("partial reply"), 1 + maxRetries},
		{"warnings", reply(200, `{"status":"success","data":{"resultType":"matrix","result":[]},"warnings":["No StoreAPIs matched `+sentinel+`"]}`),
			unavailableText("partial reply"), 1 + maxRetries},
		// Recorded limit replies: over a size limit is final, at a
		// concurrency or time limit is pushback.
		{"Prometheus max samples", reply(422, `{"status":"error","errorType":"execution","error":"query processing would load too many samples into memory in query execution `+sentinel+`"}`),
			unavailableText("HTTP 422"), 1},
		{"VictoriaMetrics max samples", reply(422, `{"status":"error","errorType":"422","error":"error when executing query=\"`+sentinel+`\": cannot select more than -search.maxSamplesPerQuery=1000000 samples"}`),
			unavailableText("HTTP 422"), 1},
		{"VictoriaMetrics max series", reply(422, `{"status":"error","errorType":"422","error":"the number of matching timeseries exceeds 300000; `+sentinel+`"}`),
			unavailableText("HTTP 422"), 1},
		{"Mimir max chunks", reply(422, `{"status":"error","errorType":"execution","error":"the query exceeded the maximum number of chunks `+sentinel+`"}`),
			unavailableText("HTTP 422"), 1},
		{"VictoriaMetrics max concurrent", reply(503, `{"status":"error","errorType":"503","error":"couldn't start executing the request in 10.000 seconds, since -search.maxConcurrentRequests=8 concurrent requests are executed `+sentinel+`"}`),
			history.Timeout{}, 1 + maxRetries},
		{"Prometheus query timeout", reply(503, `{"status":"error","errorType":"timeout","error":"query timed out in expression evaluation `+sentinel+`"}`),
			history.Timeout{}, 1 + maxRetries},
	} {
		synctest.Test(t, func(t *testing.T) {
			client, requests := served(t, Config{}, c.reply)
			_, err := client.raw(t.Context(), units, time.Now())
			eq(t, []any{err, requests.Load()}, []any{c.want, c.calls}, c.name)
		})
	}
}

// A refusal whose promised body never comes is final: its body is never
// read.
func TestARefusalWithASlowBodyIsFinal(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = listener.Close() }()
	var requests atomic.Int32
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			if _, err := http.ReadRequest(bufio.NewReader(conn)); err == nil {
				requests.Add(1)
				_, _ = conn.Write([]byte("HTTP/1.1 403 Forbidden\r\ncontent-length: 100\r\n\r\n"))
			}
			defer func() { _ = conn.Close() }()
		}
	}()
	c, _ := New(Config{URL: "http://" + listener.Addr().String()}, metadata(t), DefaultOptions)
	_, err = c.raw(t.Context(), units, time.Now())
	eq(t, []any{err, requests.Load()}, []any{unavailableText("HTTP 403"), int32(1)})
}

// A transport error is its category: never the URL or its password.
func TestATransportErrorHidesTheURL(t *testing.T) {
	c, _ := New(Config{URL: "http://127.0.0.1:1/prom"}, metadata(t), DefaultOptions)
	c.gate.spacing = time.Millisecond
	_, err := c.raw(t.Context(), units, time.Now())
	eq(t, err, unavailableText("transport error"))
}

func TestConfigDecodesCamelCaseAndRefusesUnknownKeys(t *testing.T) {
	var c Config
	err := jsonx.Decode([]byte(`{"url":"http://vmselect:8481/select/0/prometheus","bearerTokenFile":"/var/run/secrets/store/token",`+
		`"caFile":"/etc/store/ca.crt","headers":{"X-Scope-OrgID":"site-1"},"ingestionDelaySeconds":30}`), &c)
	eq(t, []any{err, c.IngestionDelaySeconds, c.Headers["X-Scope-OrgID"], *c.CAFile}, []any{nil, uint64(30), "site-1", "/etc/store/ca.crt"})
	eq(t, jsonx.Decode([]byte(`{"url":"http://prometheus:9090","bearerToken":"`+sentinel+`"}`), &c) != nil, true, "a token is never configuration")
}

func file(t *testing.T, name, content string) string {
	path := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

// The path prefix is kept, the token and the headers go with every query,
// and a rotated token is used from the first query a minute later.
func TestConfiguredStoreSendsTokenAndHeadersUnderItsPrefix(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		token := file(t, "token", "first-token\n")
		var seen []http.Header
		c, _ := served(t, Config{URL: "http://store/prom/", BearerTokenFile: &token, Headers: map[string]string{"X-Scope-OrgID": "site-1", "AccountID": "7"}},
			func(i int32, w http.ResponseWriter, r *http.Request) {
				seen = append(seen, r.Header)
				reply(200, empty)(i, w, r)
			})
		for i, content := range []string{"", "second-token", ""} {
			if content != "" {
				_ = os.WriteFile(token, []byte(content), 0o600)
			}
			if i == 2 {
				time.Sleep(tokenRefresh)
			}
			_, err := c.raw(t.Context(), units, time.Now())
			eq(t, err, nil)
		}
		var got []string
		for _, h := range seen {
			got = append(got, h.Get("Authorization")+" "+h.Get("X-Scope-OrgID")+" "+h.Get("AccountID"))
		}
		eq(t, got, []string{"Bearer first-token site-1 7", "Bearer first-token site-1 7", "Bearer second-token site-1 7"})

		// A read whose caller gave up, and that ended since, gives the next
		// query its token: no second read starts.
		c.token.readAt, c.token.reading = time.Time{}, make(chan tokenRead, 1)
		c.token.reading <- tokenRead{token: "Bearer late-token"}
		late, err := c.token.current(t.Context())
		eq(t, []any{late, err}, []any{"Bearer late-token", nil}, "the late read")
		c.token.readAt = time.Time{}
		late, err = c.token.current(t.Context())
		eq(t, []any{late, err}, []any{"Bearer second-token", nil}, "a new read")
		// A read still running fails the query at once.
		c.token.readAt, c.token.reading = time.Time{}, make(chan tokenRead, 1)
		_, err = c.token.current(t.Context())
		eq(t, err, unsupported("bearer token file cannot be read"), "a read in flight")
	})
}

func TestTokenFileProblemsNeverReachTheStore(t *testing.T) {
	refused := func(path string) error {
		_, err := New(Config{URL: "http://store", BearerTokenFile: &path}, metadata(t), DefaultOptions)
		return err
	}
	fifo := filepath.Join(t.TempDir(), "fifo")
	if err := syscall.Mkfifo(fifo, 0o600); err != nil {
		t.Fatal(err)
	}
	for _, path := range []string{filepath.Join(t.TempDir(), "missing"), fifo, t.TempDir()} {
		eq(t, refused(path), unsupported("bearer token file cannot be read"), path)
	}
	for _, bad := range []string{"", " \n", "two words", sentinel + "\x7f"} {
		eq(t, refused(file(t, "bad", bad)), unsupported("bearer token file holds no token"), bad)
	}
	// A token that goes away after the start fails the query, untried and
	// never retried: it is no store error.
	synctest.Test(t, func(t *testing.T) {
		token := file(t, "token", sentinel+"-token")
		c, requests := served(t, Config{BearerTokenFile: &token}, reply(200, empty))
		_ = os.Remove(token)
		time.Sleep(tokenRefresh)
		_, err := c.raw(t.Context(), units, time.Now())
		eq(t, []any{err, requests.Load()}, []any{unsupported("bearer token file cannot be read"), int32(0)})
	})
}

func TestConfigStringShowsNoSecret(t *testing.T) {
	path := "/var/run/secrets/" + sentinel + "/token"
	text := Config{URL: "https://probe:" + sentinel + "@store:9090/prefix?access_token=" + sentinel + "#" + sentinel,
		BearerTokenFile: &path, CAFile: &path, Headers: map[string]string{"X-Api-Key": sentinel}}.String()
	eq(t, !strings.Contains(text, sentinel) && strings.Contains(text, "X-Api-Key") && strings.Contains(text, "store:9090/prefix"), true, text)
}

// An opaque or non-http(s) URL has no hierarchical parts to strip, so String
// shows a placeholder, never the URL text that may hold a credential.
func TestConfigStringHidesAnOpaqueURL(t *testing.T) {
	for _, u := range []string{"http:" + sentinel + "@host", "mailto:" + sentinel, "http:///user:" + sentinel + "@host"} {
		eq(t, strings.Contains(Config{URL: u}.String(), sentinel), false, u)
	}
}

// The store transport is HTTP/1.1 only with keep-alives off, so net/http never
// replays a lost request and HTTP/2 cannot retry a REFUSED_STREAM.
func TestStoreTransportIsHTTP1Only(t *testing.T) {
	c, err := New(Config{URL: "http://store"}, metadata(t), DefaultOptions)
	if err != nil {
		t.Fatal(err)
	}
	tr := c.http.Transport.(*http.Transport)
	if !tr.DisableKeepAlives || tr.Protocols == nil || !tr.Protocols.HTTP1() || tr.Protocols.HTTP2() || tr.Protocols.UnencryptedHTTP2() {
		t.Errorf("protocols=%v keepAlivesDisabled=%v", tr.Protocols, tr.DisableKeepAlives)
	}
}

// A token read that cannot get the lock ends with the caller's context,
// promptly and with no store call, never after its own longer timeout.
func TestTokenCurrentHonorsTheContext(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		token := file(t, "token", sentinel+"-token")
		c, requests := served(t, Config{BearerTokenFile: &token}, reply(200, empty))
		c.token.lock <- struct{}{} // hold the lock so no read can run
		ctx, cancel := context.WithCancel(t.Context())
		time.AfterFunc(time.Second, cancel)
		started := time.Now()
		_, err := c.raw(ctx, units, time.Now())
		eq(t, []any{err, time.Since(started), requests.Load()}, []any{context.Canceled, time.Second, int32(0)})
	})
}

// A caller deadline earlier than until bounds the token-lock wait even when its
// Done is late: the call times out at +20 ms, not the token's 5 s, no store call.
func TestEarlierCallerDeadlineBoundsTheTokenWait(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		token := file(t, "token", sentinel+"-token")
		c, requests := served(t, Config{BearerTokenFile: &token}, reply(200, empty))
		c.token.lock <- struct{}{} // hold the lock so the token read cannot run
		ctx := stuckDeadline{t.Context(), time.Now().Add(20 * time.Millisecond)}
		started := time.Now()
		_, err := c.StoreStatus(ctx, time.Now().Add(time.Minute))
		eq(t, []any{err, time.Since(started), requests.Load()}, []any{error(history.Timeout{}), 20 * time.Millisecond, int32(0)})
	})
}

// lateCancel reports a cancellation whose Done has not been delivered yet.
type lateCancel struct{ context.Context }

func (lateCancel) Err() error            { return context.Canceled }
func (lateCancel) Done() <-chan struct{} { return nil }

// A caller waiting on a token read another caller started gets its own
// cancellation, not a store "cannot be read" error, even before its Done fires.
func TestTokenReadWaitHonorsCancellation(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		token := file(t, "token", sentinel+"-token")
		c, _ := served(t, Config{BearerTokenFile: &token}, reply(200, empty))
		c.token.readAt = time.Time{}           // stale: a fresh read is needed
		c.token.reading = make(chan tokenRead) // a read is in flight, no result yet
		_, err := c.token.current(lateCancel{t.Context()})
		eq(t, err, context.Canceled)
	})
}

// A cancel or an elapsed deadline while a fresh token read is pending ends the
// wait at the bound, through a public call, typed, with no store dispatch.
func TestFreshTokenReadWaitHonorsCancelAndDeadline(t *testing.T) {
	for _, c := range []struct {
		name string
		ctx  func(context.Context) (context.Context, context.CancelFunc)
		want error
	}{
		{"cancel", func(ctx context.Context) (context.Context, context.CancelFunc) {
			child, cancel := context.WithCancel(ctx)
			time.AfterFunc(20*time.Millisecond, cancel)
			return child, cancel
		}, history.Cancelled{}},
		{"deadline", func(ctx context.Context) (context.Context, context.CancelFunc) {
			return context.WithTimeout(ctx, 20*time.Millisecond)
		}, history.Timeout{}},
	} {
		synctest.Test(t, func(t *testing.T) {
			token := file(t, "token", sentinel+"-token")
			cl, requests := served(t, Config{BearerTokenFile: &token}, reply(200, empty))
			block := make(chan struct{})
			defer close(block)
			cl.token.read = func() tokenRead { <-block; return tokenRead{token: "Bearer x"} } // a stalled mount
			cl.token.readAt = time.Time{}                                                     // stale: a fresh read starts
			ctx, cancel := c.ctx(t.Context())
			defer cancel()
			started := time.Now()
			_, err := cl.StoreStatus(ctx, time.Now().Add(time.Hour))
			eq(t, []any{err, time.Since(started), requests.Load()}, []any{c.want, 20 * time.Millisecond, int32(0)}, c.name)
		})
	}
}

func TestConfigRefusesWhatItCannotSend(t *testing.T) {
	missing, notPEM := filepath.Join(t.TempDir(), "missing"), file(t, "ca", sentinel+", not a certificate")
	many := map[string]string{}
	for i := range 17 {
		many[fmt.Sprint("x-h", i)] = "v"
	}
	for _, c := range []struct {
		cfg  Config
		want string
	}{
		{Config{Headers: map[string]string{"Authorization": "Bearer " + sentinel}}, "Authorization header comes from bearerTokenFile"},
		{Config{Headers: map[string]string{"bad name": "x"}}, "header name is not valid"},
		{Config{Headers: map[string]string{"X-Scope-OrgID": sentinel + "\nInjected: yes"}}, "header x-scope-orgid is not valid"},
		{Config{Headers: many}, "takes at most 16 headers"},
		{Config{IngestionDelaySeconds: 61}, "ingestion delay is over 60s"},
		{Config{CAFile: &missing}, "CA file cannot be read"},
		{Config{CAFile: &notPEM}, "CA file holds no PEM certificate"},
		{Config{URL: "ftp://store"}, "URL is not an http(s) URL"},
		{Config{URL: "store:9090"}, "URL is not an http(s) URL"},
		// Secrets come from files only: credentials, a query or a fragment in
		// the URL, an empty host and an out-of-range port are refused in New.
		{Config{URL: "http://probe:" + sentinel + "@store"}, "URL is not an http(s) URL"},
		{Config{URL: "http://store?q=1"}, "URL is not an http(s) URL"},
		{Config{URL: "http://store#x"}, "URL is not an http(s) URL"},
		{Config{URL: "http://:9090"}, "URL is not an http(s) URL"},
		{Config{URL: "http://store:99999"}, "URL is not an http(s) URL"},
	} {
		if c.cfg.URL == "" {
			c.cfg.URL = "https://prometheus:9090"
		}
		_, err := New(c.cfg, metadata(t), DefaultOptions)
		eq(t, err, unsupported(c.want), c.want)
	}
}

// The CA file is the only trust: the system roots and another CA do not
// sign the store.
func TestCAFileIsTheOnlyTrust(t *testing.T) {
	ca := func(der []byte) string {
		return file(t, "ca.crt", string(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der})))
	}
	s := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { _, _ = w.Write([]byte(empty)) }))
	defer s.Close()
	key, _ := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	other := &x509.Certificate{SerialNumber: big.NewInt(1), IsCA: true, BasicConstraintsValid: true, NotAfter: time.Now().Add(time.Hour)}
	otherDER, _ := x509.CreateCertificate(rand.Reader, other, other, &key.PublicKey, key)
	trusted, otherCA := ca(s.Certificate().Raw), ca(otherDER)
	for _, c := range []struct {
		ca   *string
		want error
	}{{&trusted, nil}, {nil, unavailableText("transport error")}, {&otherCA, unavailableText("transport error")}} {
		client, err := New(Config{URL: s.URL, CAFile: c.ca}, metadata(t), DefaultOptions)
		if err != nil {
			t.Fatal(err)
		}
		client.gate.spacing = time.Millisecond
		_, err = client.raw(t.Context(), units, time.Now())
		eq(t, err, c.want, c.ca != nil)
	}
}
