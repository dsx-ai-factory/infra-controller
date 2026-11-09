// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package probe_test

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

const host = probeapi.UnitTypeHost

var (
	hosts = probeapi.Filter{Type: host}
	ready = probeapi.Filter{Type: host, Stage: new(probeapi.StageReady)}
)

// is reports whether err is want, nil for no error. A want with no Detail
// matches any detail. No text of err may hold a secret: want pins every
// other field.
func is(err error, want *probe.Error) bool {
	var got *probe.Error
	if err != nil && !errors.As(err, &got) {
		return false
	}
	if got != nil && want != nil && want.Detail == "" {
		c := *got
		c.Detail, got = "", &c
	}
	s := text(err)
	return reflect.DeepEqual(got, want) && !strings.Contains(s, "hunter2") &&
		!strings.Contains(s, "TOPSECRET") && !strings.Contains(s, "sentinel-host")
}

// text is the kind and detail of err, "" for none.
func text(err error) string {
	var pe *probe.Error
	if errors.As(err, &pe) {
		return strings.TrimSpace(pe.KindText() + " " + pe.Detail)
	}
	if err != nil {
		return err.Error()
	}
	return ""
}

func TestErrorClasses(t *testing.T) {
	for _, tc := range []struct {
		err                probe.Error
		kind               string
		transient, unknown bool
	}{
		{probe.Error{Kind: probe.Timeout}, "timeout", true, true},
		{probe.Error{Kind: probe.Transport}, "transport", true, true},
		{probe.Error{Kind: probe.InvalidParam}, "invalidParam", false, false},
		{probe.Error{Kind: probe.SnapshotExpired}, "snapshotExpired", true, true},
		{probe.Error{Kind: probe.Unsupported, Code: new(probeapi.ErrorCodeFieldUnavailable)}, "unsupported field_unavailable", true, true},
		{probe.Error{Kind: probe.Unsupported, Code: new(probeapi.ErrorCodeUnsupportedFilter)}, "unsupported unsupported_filter", false, true},
		{probe.Error{Kind: probe.Busy}, "busy", true, true},
		{probe.Error{Kind: probe.NoCompleteSweep}, "noCompleteSweep", true, true},
		{probe.Error{Kind: probe.Status, HTTPStatus: 500}, "status 500", true, true},
		{probe.Error{Kind: probe.Status, HTTPStatus: 501, Code: new(probeapi.ErrorCodeInternal)}, "status 501", true, true},
		{probe.Error{Kind: probe.Status, HTTPStatus: 400}, "status 400", false, true},
		{probe.Error{Kind: probe.Status, HTTPStatus: 404}, "status 404", false, true},
		{probe.Error{Kind: probe.Decode}, "decode", false, true},
		{probe.Error{Kind: probe.Unavailable}, "unavailable", true, true},
		{probe.Error{Kind: probe.Cancelled}, "cancelled", false, true},
	} {
		if got := [3]any{tc.err.KindText(), tc.err.Transient(), tc.err.Unknown()}; got != [3]any{tc.kind, tc.transient, tc.unknown} {
			t.Errorf("%s: got %v", tc.kind, got)
		}
	}
}

const (
	countJSON = `{"count":3,"snapshotId":"s1","family":"host","sweepStart":"2026-10-23T09:59:35Z",` +
		`"readAt":"2026-10-23T09:59:55Z","sweepAgeSeconds":5.0,"sweepIntervalSeconds":20.0}`
	gap  = `{"error":{"code":"field_unavailable","message":"m","field":"f"}}`
	html = `<html>http://user:hunter2@sentinel-host/x?t=1 Authorization: Bearer TOPSECRET</html>`
)

func errorBody(code, message, field string) string {
	return `{"error":{"code":"` + code + `","message":"` + message + `","field":"` + field + `"}}`
}

func pad(s string, n int) string { return s + strings.Repeat(" ", n-len(s)) }

type roundTrip func(*http.Request) (*http.Response, error)

func (f roundTrip) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

// eofAfter runs at the end of a reply body, after its last byte was read.
type eofAfter func()

func (f eofAfter) Read([]byte) (int, error) { f(); return 0, io.EOF }

// Each row is a raw reply to a count: its status, Retry-After and body, then
// what the body does after it.
func TestReplies(t *testing.T) {
	unsupported := func(c probeapi.ErrorCode, field *string) *probe.Error {
		return &probe.Error{Kind: probe.Unsupported, Code: &c, Field: field}
	}
	status := func(s int, c *probeapi.ErrorCode) *probe.Error {
		return &probe.Error{Kind: probe.Status, HTTPStatus: s, Code: c}
	}
	refused := unsupported(probeapi.ErrorCodeUnsupportedFilter, nil)
	tooBig := &probe.Error{Kind: probe.Decode, Detail: "reply body is over 16777216 bytes"}
	invalid := &probe.Error{Kind: probe.InvalidParam}
	busy := func(s int) *probe.Error {
		return &probe.Error{Kind: probe.Busy, RetryAfter: time.Duration(s) * time.Second}
	}
	noSweep := func(s int) *probe.Error {
		return &probe.Error{Kind: probe.NoCompleteSweep, RetryAfter: time.Duration(s) * time.Second}
	}
	for _, tc := range []struct {
		name, retryAfter, body string
		status                 int
		end                    string // "stall", "reset", "endless", "cancel" or "late" after the body, "upgrade" for none
		length                 int    // an announced Content-Length
		want                   *probe.Error
	}{
		{name: "a count", status: 200, body: countJSON},
		{name: "a count at exactly the cap", status: 200, body: pad(countJSON, 16<<20)},
		{name: "a 2xx over the cap", status: 200, body: pad(countJSON, 16<<20+1), want: tooBig},
		{name: "a 2xx announced over the cap is refused before its body", status: 200, body: countJSON,
			end: "stall", length: 17 << 20, want: tooBig},
		{name: "a 3xx with a count is not a reply", status: 302, body: countJSON, want: status(302, nil)},
		{name: "a 101 is refused unread and its stream closed", end: "upgrade", want: status(101, nil)},
		{name: "empty 422", status: 422, want: refused},
		{name: "malformed 422", status: 422, body: `{"error":{"code":"unsupported_filter"`, want: refused},
		{name: "HTML 422", status: 422, body: html, want: refused},
		{name: "422 with its error body", status: 422, body: errorBody("field_unavailable", "m", "allocatable"),
			want: unsupported(probeapi.ErrorCodeFieldUnavailable, new("allocatable"))},
		{name: "422 with a byte that is not UTF-8", status: 422,
			body: `{"error":{"code":"field_unavailable","message":"b` + "\xff" + `d","field":"f"}}`, want: refused},
		{name: "an error body at the cap", status: 422, body: pad(gap, 64<<10),
			want: unsupported(probeapi.ErrorCodeFieldUnavailable, nil)},
		{name: "an error body over the cap", status: 422, body: pad(gap, 64<<10+1), want: refused},
		{name: "an error body that never ends", status: 422, body: gap, end: "endless", want: refused},
		{name: "empty 408", status: 408, want: &probe.Error{Kind: probe.Timeout}},
		{name: "408 with a body", status: 408, body: "request timeout", want: &probe.Error{Kind: probe.Timeout}},
		{name: "empty 400", status: 400, want: status(400, nil)},
		{name: "400 with secrets", status: 400, body: errorBody("invalid_param", html, html), want: invalid},
		{name: "400 names a known field", status: 400, body: errorBody("invalid_param", "password=TOPSECRET", "limit"),
			want: &probe.Error{Kind: probe.InvalidParam, Field: new("limit")}},
		{name: "500 keeps only its code", status: 500, body: errorBody("internal", "http://sentinel-host", "TOPSECRET"),
			want: status(500, new(probeapi.ErrorCodeInternal))},
		{name: "502 HTML", status: 502, body: html, want: status(502, nil)},
		{name: "429 with Retry-After", status: 429, retryAfter: "7", want: busy(7)},
		{name: "503 with Retry-After", status: 503, retryAfter: "7", want: noSweep(7)},
		{name: "bodiless 503 from a proxy", status: 503, want: noSweep(0)},
		{name: "Retry-After over the cap", status: 503, retryAfter: "3600", want: noSweep(60)},
		{name: "Retry-After 0 is 1 s", status: 429, retryAfter: "0", want: busy(1)},
		{name: "Retry-After as a date", status: 503, retryAfter: "Wed, 21 Oct 2026 07:28:00 GMT", want: noSweep(0)},
		{name: "403 stalled", status: 403, body: `{"err`, end: "stall", want: status(403, nil)},
		{name: "403 reset", status: 403, body: `{"err`, end: "reset", want: status(403, nil)},
		{name: "409 reset", status: 409, body: `{"err`, end: "reset", want: &probe.Error{Kind: probe.SnapshotExpired}},
		{name: "503 stalled", status: 503, body: `{"err`, end: "stall", want: noSweep(0)},
		{name: "422 whole error, stalled", status: 422, body: gap, end: "stall", want: refused},
		{name: "422 whole error, reset", status: 422, body: gap, end: "reset", want: refused},
		{name: "2xx stalled", status: 200, body: `{"err`, end: "stall", want: &probe.Error{Kind: probe.Timeout}},
		{name: "2xx reset", status: 200, body: `{"err`, end: "reset",
			want: &probe.Error{Kind: probe.Transport, Detail: "reply body failed"}},
		{name: "2xx not JSON", status: 200, body: "http://user:hunter2@sentinel-host nope", want: &probe.Error{Kind: probe.Decode}},
		{name: "2xx ID with a byte that is not UTF-8", status: 200, body: strings.Replace(countJSON, "s1", "s\xff", 1),
			want: &probe.Error{Kind: probe.Decode}},
		{name: "a 2xx read whole after a cancel", status: 200, body: countJSON, end: "cancel", want: &probe.Error{Kind: probe.Cancelled}},
		{name: "a 2xx read whole after the timeout", status: 200, body: countJSON, end: "late", want: &probe.Error{Kind: probe.Timeout}},
		{name: "a refusal read whole after a cancel keeps its status", status: 422, body: gap, end: "cancel",
			want: unsupported(probeapi.ErrorCodeFieldUnavailable, nil)},
	} {
		t.Run(tc.name, func(t *testing.T) {
			closed := make(chan error, 1)
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if tc.end == "upgrade" { // the stream stays open until the client closes it, 5 s at most
					conn, rw, _ := w.(http.Hijacker).Hijack()
					defer func() { _ = conn.Close() }()
					_, _ = rw.WriteString("HTTP/1.1 101 Switching Protocols\r\nUpgrade: x\r\nConnection: Upgrade\r\n\r\n")
					_ = rw.Flush()
					_ = conn.SetReadDeadline(time.Now().Add(5 * time.Second))
					_, err := conn.Read(make([]byte, 1))
					closed <- err
					return
				}
				w.Header().Set("Location", "/") // followed, it would loop
				if tc.retryAfter != "" {
					w.Header().Set("Retry-After", tc.retryAfter)
				}
				if tc.length > 0 {
					w.Header().Set("Content-Length", strconv.Itoa(tc.length))
				}
				w.WriteHeader(tc.status)
				_, _ = io.WriteString(w, tc.body)
				w.(http.Flusher).Flush()
				switch tc.end {
				case "stall":
					<-r.Context().Done()
				case "reset":
					panic(http.ErrAbortHandler)
				case "endless":
					for r.Context().Err() == nil {
						_, _ = io.WriteString(w, pad("", 1<<16))
					}
				}
			}))
			defer srv.Close()
			wait := time.Minute // only a stall within the cap or a late body may wait on the read timeout
			if tc.end == "stall" && tc.length == 0 || tc.end == "late" {
				wait = 300 * time.Millisecond
			}
			p, err := probe.NewHTTP(srv.URL, wait)
			if err != nil {
				t.Fatal(err)
			}
			// A cancel or a wait past the timeout comes after the last byte of
			// the body is read, where no read can fail.
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			if after := map[string]func(){"cancel": cancel, "late": func() { time.Sleep(2 * wait) }}[tc.end]; after != nil {
				probe.SetTransport(p, roundTrip(func(*http.Request) (*http.Response, error) {
					body := io.MultiReader(strings.NewReader(tc.body), eofAfter(after))
					return &http.Response{StatusCode: tc.status, Body: io.NopCloser(body)}, nil
				}))
			}
			start := time.Now()
			c, err := p.Count(ctx, ready)
			if !is(err, tc.want) || tc.want == nil && c.Count != 3 || time.Since(start) > max(wait/2, 2*time.Second) {
				t.Errorf("got %d, %+v after %v, want %+v", c.Count, err, time.Since(start), tc.want)
			}
			if tc.end == "upgrade" && <-closed != io.EOF {
				t.Error("the upgraded stream stayed open")
			}
		})
	}
}

// Each endpoint builds its request and decodes a populated reply. The
// recording handler pins method, path, query and body; the digest pins the
// decoded fields, so a zero-valued reply with no error fails.
func TestRequestsAndRepliesOverHTTP(t *testing.T) {
	var got struct{ method, path, query, body string }
	units := `{"snapshotId":"s1","family":"host","sweepStart":"2026-10-23T09:59:35Z","readAt":"2026-10-23T09:59:55Z",` +
		`"sweepAgeSeconds":5,"sweepIntervalSeconds":20,"units":[{"id":"fm100h","type":"host","source":"nico-api",` +
		`"stage":"ready","done":true,"state":"Ready","reason":null,"bmcIp":null,"host":null,"dpu":null,"instance":null,` +
		`"expected":null}],"missingIds":["gone"],"nextCursor":null}`
	config := `{"nicoApi":null,"rest":null,"restSite":null,"metrics":{"contractVersion":7,"selector":{"job":"p"},` +
		`"scrapeIntervalSeconds":1,"readerIntervalsSeconds":{},"processStartSeconds":1}}`
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		b, _ := io.ReadAll(r.Body)
		got.method, got.path, got.query, got.body = r.Method, r.URL.Path, r.URL.RawQuery, string(b)
		switch r.URL.Path {
		case "/v1/count":
			_, _ = io.WriteString(w, countJSON)
		case "/v1/config":
			_, _ = io.WriteString(w, config)
		default:
			_, _ = io.WriteString(w, units)
		}
	}))
	defer srv.Close()
	p, err := probe.NewHTTP(srv.URL, time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	model, limit, cursor := "GB200 NVL", uint32(50), "c0"
	for _, tc := range []struct {
		name, method, path, query, body, want string
		do                                    func() (string, error)
	}{
		{"count", "GET", "/v1/count", "stage=ready&type=host", "", "3",
			func() (string, error) { c, e := p.Count(ctx, ready); return fmt.Sprint(c.Count), e }},
		{"units", "GET", "/v1/units", "cursor=c0&limit=50&model=GB200+NVL&type=host", "", "[fm100h] [gone]",
			func() (string, error) {
				u, e := p.Units(ctx, probeapi.Filter{Type: host, Model: &model}, nil, probe.Page{Limit: &limit, Cursor: &cursor})
				return fmt.Sprint(ids(u.Units), u.MissingIDs), e
			}},
		{"units/query", "POST", "/v1/units/query", "", `{"type":"host","ids":["a"]}`, "[fm100h] [gone]",
			func() (string, error) {
				u, e := p.UnitsQuery(ctx, probeapi.UnitsQuery{Filter: probeapi.Filter{Type: host}, IDs: []string{"a"}})
				return fmt.Sprint(ids(u.Units), u.MissingIDs), e
			}},
		{"config", "GET", "/v1/config", "component=bmc&model=R760", "", "7",
			func() (string, error) {
				c, e := p.Config(ctx, probe.ConfigFilter{Model: new("R760"), Component: new(probeapi.ComponentBmc)})
				if e != nil || c.Metrics == nil {
					return "no metrics", e
				}
				return fmt.Sprint(c.Metrics.ContractVersion), e
			}},
	} {
		digest, err := tc.do()
		if err != nil {
			t.Errorf("%s: %v", tc.name, err)
			continue
		}
		if got.method != tc.method || got.path != tc.path || got.query != tc.query || got.body != tc.body {
			t.Errorf("%s: got %s %s?%s body %q", tc.name, got.method, got.path, got.query, got.body)
		}
		if digest != tc.want {
			t.Errorf("%s: decoded %q, want %q", tc.name, digest, tc.want)
		}
	}
}

// An oversized 2xx that arrives after a cancel or an elapsed deadline is the
// stop, not a Decode: the stop outranks the size verdict on every 2xx exit.
func TestAnOversized2xxAfterAStopIsTheStop(t *testing.T) {
	big := int64(16<<20) + 1
	for _, tc := range []struct {
		name     string
		header   bool // oversize by Content-Length, else by an over-cap body
		deadline bool // elapse the per-call deadline, else cancel
		want     *probe.Error
	}{
		{"content-length, cancel", true, false, &probe.Error{Kind: probe.Cancelled}},
		{"over-cap read, cancel", false, false, &probe.Error{Kind: probe.Cancelled}},
		{"content-length, deadline", true, true, &probe.Error{Kind: probe.Timeout}},
		{"over-cap read, deadline", false, true, &probe.Error{Kind: probe.Timeout}},
	} {
		timeout := time.Minute
		if tc.deadline {
			timeout = 50 * time.Millisecond
		}
		p, _ := probe.NewHTTP("http://probe", timeout)
		ctx, cancel := context.WithCancel(context.Background())
		probe.SetTransport(p, roundTrip(func(r *http.Request) (*http.Response, error) {
			if !tc.deadline {
				cancel()
			}
			<-r.Context().Done() // the per-call ctx is done before the reply returns
			resp := &http.Response{StatusCode: 200, ContentLength: -1, Body: io.NopCloser(strings.NewReader("x"))}
			if tc.header {
				resp.ContentLength = big
			} else {
				resp.Body = io.NopCloser(strings.NewReader(strings.Repeat("x", int(big))))
			}
			return resp, nil
		}))
		_, err := p.Count(ctx, ready)
		cancel()
		if !is(err, tc.want) {
			t.Errorf("%s: got %v, want %v", tc.name, err, tc.want)
		}
	}
}

// A GET whose reply is lost on a connection is not replayed, so the probe
// sees one request per call however net/http pools connections.
func TestNoReplayWhenAReplyIsLost(t *testing.T) {
	var reqs atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if reqs.Add(1) == 1 { // warm the client with one real reply
			_, _ = io.WriteString(w, countJSON)
			return
		}
		conn, _, _ := w.(http.Hijacker).Hijack() // later: read the request, drop the reply
		_ = conn.Close()
	}))
	defer srv.Close()
	p, err := probe.NewHTTP(srv.URL, 5*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	if _, err := p.Count(ctx, ready); err != nil {
		t.Fatalf("warm: %v", err)
	}
	if _, err := p.Count(ctx, ready); err == nil {
		t.Error("a lost reply must be an error")
	}
	if n := reqs.Load(); n != 2 {
		t.Errorf("the probe saw %d requests, want 2; a replay makes 3", n)
	}
}

// The constructor enforces the config's URL policy and a positive timeout,
// and makes no request. A userinfo URL leaks nothing.
func TestNewHTTPRejectsBadURLsAndTimeouts(t *testing.T) {
	for _, tc := range []struct {
		base    string
		timeout time.Duration
		detail  string
	}{
		{"http://probe", 0, "the probe timeout must be over zero"},
		{"http://probe", -time.Second, "the probe timeout must be over zero"},
		{"ftp://probe", time.Second, "the probe URL is not http or https with a host"},
		{"http://", time.Second, "the probe URL is not http or https with a host"},
		{"http://probe:70000", time.Second, "the probe URL is not http or https with a host"},
		{"http://user:hunter2@probe", time.Second, "the probe URL must hold no user name or password"},
		{"http://probe/?token=x", time.Second, "the probe URL must hold no query or fragment"},
		{"http://probe/#x", time.Second, "the probe URL must hold no query or fragment"},
		{"http://probe/base", time.Second, "the probe URL must hold no path"},
	} {
		p, err := probe.NewHTTP(tc.base, tc.timeout)
		if p != nil || !is(err, &probe.Error{Kind: probe.InvalidParam, Detail: tc.detail}) {
			t.Errorf("%s/%v: got %v, want %q", tc.base, tc.timeout, err, tc.detail)
		}
	}
	if p, err := probe.NewHTTP("http://probe:9010/", time.Second); err != nil || p == nil {
		t.Errorf("a host with a root path was refused: %v", err)
	}
}

// A connect failure keeps only its kind. No error holds the URL, its user or
// the query.
func TestTransportErrorsLeakNoURL(t *testing.T) {
	gone, _ := probe.NewHTTP("http://127.0.0.1:0", time.Minute)
	needle := probeapi.Filter{Type: host, State: new("needle-state")}
	_, err1 := gone.Count(context.Background(), needle)
	_, err2 := gone.Units(context.Background(), needle, new("needle-name"), probe.Page{})
	for _, err := range []error{err1, err2} {
		got := text(err)
		if !is(err, &probe.Error{Kind: probe.Transport, Detail: "connection failed"}) ||
			slices.ContainsFunc([]string{"user", "127.0.0.1", "needle", "/v1/"}, func(s string) bool { return strings.Contains(got, s) }) {
			t.Errorf("got %s", got)
		}
	}
}

// stuckDeadline has a deadline whose Done never closes, as a context whose
// timer callback has not run yet.
type stuckDeadline struct {
	context.Context
	at time.Time
}

func (c stuckDeadline) Deadline() (time.Time, bool) { return c.at, true }

// errReader fails after returning some bytes: a body that truncates.
type errReader struct{}

func (errReader) Read([]byte) (int, error) { return 0, io.ErrUnexpectedEOF }

// A call that finishes after the deadline is a timeout even while the
// context's timer has not run: a whole reply, a body that truncates part
// way, or a transport error from the round trip.
func TestAReplyAfterTheDeadlineIsATimeout(t *testing.T) {
	for _, tc := range []struct {
		name string
		rt   roundTrip
	}{
		{"whole body", func(*http.Request) (*http.Response, error) {
			return &http.Response{StatusCode: 200, ContentLength: -1, Body: io.NopCloser(strings.NewReader(countJSON))}, nil
		}},
		{"truncated body", func(*http.Request) (*http.Response, error) {
			body := io.MultiReader(strings.NewReader(countJSON[:10]), errReader{})
			return &http.Response{StatusCode: 200, ContentLength: -1, Body: io.NopCloser(body)}, nil
		}},
		{"transport error", func(*http.Request) (*http.Response, error) { return nil, io.ErrUnexpectedEOF }},
	} {
		p, _ := probe.NewHTTP("http://probe", time.Minute)
		probe.SetTransport(p, tc.rt)
		// The deadline is already past, but the context's Done is never closed,
		// so only the elapsed-deadline check can report it.
		ctx := stuckDeadline{context.Background(), time.Now().Add(-time.Second)}
		if _, err := p.Count(ctx, ready); !is(err, &probe.Error{Kind: probe.Timeout}) {
			t.Errorf("%s: got %v, want a timeout", tc.name, err)
		}
	}
}

// The built transport is HTTP/1.1 only with keep-alives off, so neither the
// HTTP/1 reuse replay nor the HTTP/2 REFUSED_STREAM retry can fire.
func TestTransportIsHTTP1Only(t *testing.T) {
	tr := probe.HTTPTransport(mustProbe(t, "http://probe"))
	if !tr.DisableKeepAlives || tr.Protocols == nil || !tr.Protocols.HTTP1() ||
		tr.Protocols.HTTP2() || tr.Protocols.UnencryptedHTTP2() {
		t.Errorf("transport protocols=%v keepAlivesDisabled=%v", tr.Protocols, tr.DisableKeepAlives)
	}
}

func mustProbe(t *testing.T, base string) probe.Probe {
	t.Helper()
	p, err := probe.NewHTTP(base, time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// A request this client cannot make is its own fault, never retried.
func TestLocalFailuresAreFinal(t *testing.T) {
	for _, base := range []string{"mailto:probe@example.com", "ftp://127.0.0.1:1/", "http://", "http://[::1"} {
		_, err := probe.NewHTTP(base, time.Second)
		if !is(err, &probe.Error{Kind: probe.InvalidParam, Detail: "the probe URL is not http or https with a host"}) {
			t.Errorf("%s: %v", base, err)
		}
	}
	p, _ := probe.NewHTTP("http://127.0.0.1:0", time.Second)
	bad := probeapi.Filter{Type: "bogus"}
	_, err1 := p.Count(context.Background(), bad)
	_, err2 := p.Units(context.Background(), bad, nil, probe.Page{})
	_, err3 := p.UnitsQuery(context.Background(), probeapi.UnitsQuery{Filter: bad})
	for _, err := range []error{err1, err2, err3} {
		var pe *probe.Error
		if !is(err, &probe.Error{Kind: probe.InvalidParam, Detail: "the request could not be built"}) || !errors.As(err, &pe) || pe.Transient() {
			t.Errorf("got %v", err)
		}
	}
}

func unit(id string) probeapi.Unit {
	return probeapi.Unit{ID: id, Type: host, Source: probeapi.SourceNicoAPI, Stage: probeapi.StageReady, Done: true, State: "Ready"}
}

type reply struct {
	r   probeapi.UnitsResponse
	err error
}

// scripted answers Units call n (from 0) with next. It serves nothing else.
type scripted struct {
	probe.Probe
	next  func(ctx context.Context, n int, p probe.Page) reply
	calls int
}

func (s *scripted) Units(ctx context.Context, _ probeapi.Filter, _ *string, p probe.Page) (probeapi.UnitsResponse, error) {
	s.calls++
	r := s.next(ctx, s.calls-1, p)
	return r.r, r.err
}

// page is a host page of snapshot, age seconds old on a 20 s interval.
func page(snapshot string, age float64, next string, ids ...string) reply {
	r := probeapi.UnitsResponse{SweepInfo: probeapi.SweepInfo{SnapshotID: snapshot, Family: probeapi.FamilyHost,
		SweepAgeSeconds: age, SweepIntervalSeconds: 20}, Units: []probeapi.Unit{}, MissingIDs: []string{}}
	for _, id := range ids {
		r.Units = append(r.Units, unit(id))
	}
	if next != "" {
		r.NextCursor = &next
	}
	return reply{r: r}
}

// pages answers with replies in order, then 500.
func pages(replies ...reply) func(context.Context, int, probe.Page) reply {
	return func(_ context.Context, n int, _ probe.Page) reply {
		if n < len(replies) {
			return replies[n]
		}
		return reply{err: &probe.Error{Kind: probe.Status, HTTPStatus: 500}}
	}
}

var expired = reply{err: &probe.Error{Kind: probe.SnapshotExpired}}

// serveAll pages all in blocks of 1000 under snapshot, 409 on the calls in
// exp. A restart resets the cursor, so the same snapshot is read again.
func serveAll(all []string, snapshot string, exp ...int) func(context.Context, int, probe.Page) reply {
	return func(_ context.Context, n int, p probe.Page) reply {
		if slices.Contains(exp, n) {
			return expired
		}
		off := 0
		if p.Cursor != nil {
			off, _ = strconv.Atoi(strings.TrimPrefix(*p.Cursor, snapshot+":"))
		}
		end := min(off+1000, len(all))
		next := ""
		if end < len(all) {
			next = fmt.Sprintf("%s:%d", snapshot, end)
		}
		return page(snapshot, 5, next, all[off:end]...)
	}
}

func ids(units []probeapi.Unit) (ids []string) {
	for _, u := range units {
		ids = append(ids, u.ID)
	}
	return ids
}

func TestAllUnitsPaging(t *testing.T) {
	var all []string
	for i := range 2500 {
		all = append(all, fmt.Sprintf("h%05d", i))
	}
	stale, two := "unavailable the host sweep is 41 s old, interval 20 s", "unavailable pages came from two snapshots"
	passes := pages(page("s1", 5, "c1", "a"), page("s1", 5, "c2", "b"), expired,
		page("s2", 5, "c1", "a"), page("s2", 5, "c2", "b"), page("s2", 5, "", "c"))
	for _, tc := range []struct {
		name   string
		budget [2]int // pages and units, none for AllUnits
		next   func(context.Context, int, probe.Page) reply
		want   []string
		err    string
		calls  int
	}{
		{name: "pages of 1000, every unit once and in order over a restart", next: serveAll(all, "s1", 1), want: all, calls: 5},
		{name: "a second 409 is returned", next: serveAll(all, "s1", 1, 3), err: "snapshotExpired", calls: 4},
		{name: "fresh pages of one snapshot", next: pages(page("s1", 5, "c1", "a"), page("s1", 40, "", "b")),
			want: []string{"a", "b"}, calls: 2},
		{name: "a stale first page", next: pages(page("s1", 41, "c1", "a")), err: stale, calls: 1},
		{name: "a later page goes stale", next: pages(page("s1", 5, "c1", "a"), page("s1", 41, "", "b")), err: stale, calls: 2},
		{name: "the snapshot changes between pages", next: pages(page("s1", 5, "c1", "a"), page("s2", 5, "", "b")),
			err: two, calls: 2},
		{name: "a 1 MiB snapshot ID stays out of the error", err: two, calls: 2,
			next: pages(page("s1", 5, "c1", "a"), page(strings.Repeat("x", 1<<20), 5, "", "b"))},
		{name: "the one restart may read a new snapshot", next: pages(page("s1", 5, "c1", "a"), expired, page("s2", 5, "", "b")),
			want: []string{"b"}, calls: 3},
		{name: "a cursor that comes back twice", err: "unavailable a cursor came back twice", calls: 2,
			next: func(_ context.Context, n int, _ probe.Page) reply { return page("s1", 5, "same", fmt.Sprint("u", n)) }},
		{name: "an empty page that asks for more", next: pages(page("s1", 5, "c1")),
			err: "unavailable an empty page asked for more", calls: 1},
		{name: "new cursors for ever stop at the page budget", budget: [2]int{5, 100}, calls: 5,
			err: "unavailable the read needs over 5 pages",
			next: func(_ context.Context, n int, _ probe.Page) reply {
				return page("s1", 5, fmt.Sprint("c", n), fmt.Sprint("u", n))
			}},
		{name: "the unit budget", budget: [2]int{100, 3}, err: "unavailable the read holds over 3 units", calls: 2,
			next: func(_ context.Context, n int, _ probe.Page) reply { return page("s1", 5, fmt.Sprint("c", n), "a", "b") }},
		{name: "the budget spans the restart", budget: [2]int{5, 100}, next: passes, want: []string{"a", "b", "c"}, calls: 6},
		{name: "the restart does not reset the budget", budget: [2]int{4, 100}, next: passes,
			err: "unavailable the read needs over 4 pages", calls: 5},
	} {
		p, far := &scripted{next: tc.next}, time.Now().Add(time.Hour)
		var units []probeapi.Unit
		var err error
		if tc.budget == ([2]int{}) {
			units, err = probe.AllUnits(context.Background(), p, far, hosts, nil)
		} else {
			units, err = probe.AllUnitsWithin(context.Background(), p, far, hosts, nil, tc.budget[0], tc.budget[1])
		}
		if !slices.Equal(ids(units), tc.want) || text(err) != tc.err || p.calls != tc.calls {
			t.Errorf("%s: got %d units, %q, %d calls", tc.name, len(units), text(err), p.calls)
		}
	}
	if probe.ReadBudget != [2]int{402, 400_000} {
		t.Errorf("read budget %v", probe.ReadBudget)
	}
}

// blocker's Units blocks until its context ends, so bounded's deadline or a
// cancel decides the read.
type blocker struct{ probe.Probe }

func (blocker) Units(ctx context.Context, _ probeapi.Filter, _ *string, _ probe.Page) (probeapi.UnitsResponse, error) {
	<-ctx.Done()
	return probeapi.UnitsResponse{}, &probe.Error{Kind: probe.Cancelled}
}

func TestAllUnitsStops(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		for _, tc := range []struct {
			name                    string
			cancelled, cancelInCall bool
			until                   time.Duration
			want                    string
			calls                   int
		}{
			{name: "past the deadline a page ready at once is not asked for", want: "timeout"},
			{name: "cancelled before the read", cancelled: true, until: time.Hour, want: "cancelled"},
			{name: "cancelled and past the deadline: cancel first", cancelled: true, want: "cancelled"},
			{name: "a cancel while the reply is made drops it", cancelInCall: true, until: time.Hour, want: "cancelled", calls: 1},
			{name: "nothing stopped", until: time.Hour, calls: 1},
		} {
			ctx, cancel := context.WithCancel(context.Background())
			if tc.cancelled {
				cancel()
			}
			p := &scripted{next: func(context.Context, int, probe.Page) reply {
				if tc.cancelInCall {
					cancel()
				}
				return page("s1", 5, "", "a")
			}}
			units, err := probe.AllUnits(ctx, p, time.Now().Add(tc.until), hosts, nil)
			if text(err) != tc.want || p.calls != tc.calls || err == nil && len(units) != 1 {
				t.Errorf("%s: got %q, %d calls", tc.name, text(err), p.calls)
			}
			cancel()
		}

		// A page that never comes ends the read at the deadline.
		start := time.Now()
		if _, err := probe.AllUnits(context.Background(), blocker{}, start.Add(30*time.Second), hosts, nil); text(err) != "timeout" ||
			time.Since(start) != 30*time.Second {
			t.Errorf("held: %q after %v", text(err), time.Since(start))
		}
		// A cancel ends a blocked read at once.
		ctx, cancel := context.WithCancel(context.Background())
		errc := make(chan error, 1)
		go func() { _, err := probe.AllUnits(ctx, blocker{}, time.Now().Add(time.Hour), hosts, nil); errc <- err }()
		synctest.Wait()
		start = time.Now()
		cancel()
		if err := <-errc; text(err) != "cancelled" || time.Since(start) != 0 {
			t.Errorf("cancel: %q after %v", text(err), time.Since(start))
		}
	})
}
