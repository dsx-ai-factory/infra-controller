// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventory

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

// tlsServed is an HTTPS server with a self-signed cert at the records route, and a reader whose transport accepts it.
func tlsServed(t *testing.T, h http.HandlerFunc) (*HTTPReader, string) {
	srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { h(w, r) }))
	t.Cleanup(srv.Close)
	return NewHTTPReader(), srv.URL + RecordsPath
}

type roundTrip func(*http.Request) (*http.Response, error)

func (f roundTrip) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

// bodySpy counts reads and whether it was closed.
type bodySpy struct {
	reads  int
	closed bool
}

func (b *bodySpy) Read([]byte) (int, error) { b.reads++; return 0, io.EOF }
func (b *bodySpy) Close() error             { b.closed = true; return nil }

func readErr(t *testing.T, err error) *ReadError {
	t.Helper()
	re, ok := err.(*ReadError)
	if !ok {
		t.Fatalf("want a *ReadError, got %T: %v", err, err)
	}
	return re
}

func TestHTTPReader(t *testing.T) {
	t.Run("a 200 decodes and a non-2xx is named by status", func(t *testing.T) {
		r, url := tlsServed(t, func(w http.ResponseWriter, _ *http.Request) {
			_, _ = io.WriteString(w, `{"inventoryId":"inv-1","registerExpectedMachines":false,"carbideApiUrl":"https://n","racks":[{"rackId":"r1","rackProfileId":"p","group":{"rackGroupId":"g","topology":"t","protocol":"p","members":[]}}],"devices":[],"dpus":[]}`)
		})
		resp, err := r.Read(bg, url)
		is(t, []any{resp.InventoryID, len(resp.Racks), err}, []any{"inv-1", 1, error(nil)}, "decoded")

		r2, url2 := tlsServed(t, func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(404) })
		_, err = r2.Read(bg, url2)
		is(t, []any{err.Error(), readErr(t, err).Kind}, []any{"status 404", ReadStatus}, "status named")
	})

	t.Run("only transient statuses are tried again", func(t *testing.T) {
		for _, tc := range []struct {
			code  int
			tries int32
		}{{503, 2}, {429, 2}, {408, 2}, {400, 1}, {404, 1}} {
			var hits atomic.Int32
			r, url := tlsServed(t, func(w http.ResponseWriter, _ *http.Request) {
				hits.Add(1)
				w.WriteHeader(tc.code)
			})
			_, _ = r.Read(bg, url)
			is(t, hits.Load(), tc.tries, http.StatusText(tc.code))
		}
	})

	t.Run("a non-2xx is named before its body is read", func(t *testing.T) {
		r := NewHTTPReader()
		spy := &bodySpy{}
		r.client.Transport = roundTrip(func(*http.Request) (*http.Response, error) {
			return &http.Response{StatusCode: 400, Body: spy, Header: http.Header{}}, nil
		})
		_, err := r.Read(bg, "https://pod/records")
		is(t, []any{err.Error(), spy.reads, spy.closed}, []any{"status 400", 0, true}, "status, body untouched, closed")
	})

	t.Run("a reply over the cap is refused", func(t *testing.T) {
		r, url := tlsServed(t, func(w http.ResponseWriter, _ *http.Request) {
			_, _ = io.WriteString(w, strings.Repeat("x", 200))
		})
		r.max = 50
		_, err := r.Read(bg, url)
		is(t, []any{err.Error(), readErr(t, err).Kind}, []any{"the reply is over 50 bytes", ReadOversize}, "capped")
	})

	t.Run("a bad body names the decode fault, not its bytes", func(t *testing.T) {
		r, url := tlsServed(t, func(w http.ResponseWriter, _ *http.Request) {
			_, _ = io.WriteString(w, `{"racks":"SECRET"}`)
		})
		_, err := r.Read(bg, url)
		re := readErr(t, err)
		if re.Kind != ReadDecode || strings.Contains(re.Detail, "SECRET") {
			t.Errorf("decode detail leaked or wrong kind: %+v", re)
		}
	})

	t.Run("the reader decodes strictly, not with encoding/json", func(t *testing.T) {
		r, url := tlsServed(t, func(w http.ResponseWriter, _ *http.Request) { // duplicate field: json.Unmarshal accepts, jsonx refuses
			_, _ = io.WriteString(w, `{"registerExpectedMachines":false,"registerExpectedMachines":true,"inventoryId":"i","carbideApiUrl":"https://n","racks":[],"devices":[],"dpus":[]}`)
		})
		_, err := r.Read(bg, url)
		is(t, readErr(t, err).Kind, ReadDecode, "duplicate key refused")
	})

	t.Run("transport failures are named by kind, never the url", func(t *testing.T) {
		r := NewHTTPReader()
		_, err := r.Read(bg, "https://127.0.0.1:0"+RecordsPath)
		if re := readErr(t, err); re.Kind != ReadTransport || strings.Contains(re.Detail, "127.0.0.1") {
			t.Errorf("want a transport kind without the url, got %+v", re)
		}

		slow, slowURL := tlsServed(t, func(w http.ResponseWriter, _ *http.Request) {
			time.Sleep(200 * time.Millisecond)
			_, _ = io.WriteString(w, "{}")
		})
		slow.timeout = 20 * time.Millisecond
		_, err = slow.Read(bg, slowURL)
		is(t, []any{err.Error(), readErr(t, err).Kind}, []any{"timed out", ReadTimeout}, "connect timeout")
	})

	t.Run("headers and a prefix then a body timeout is a transient timeout", func(t *testing.T) {
		var hits atomic.Int32
		r, url := tlsServed(t, func(w http.ResponseWriter, req *http.Request) {
			hits.Add(1)
			w.WriteHeader(200)
			_, _ = io.WriteString(w, `{"inventoryId":`) // a prefix, then stall
			w.(http.Flusher).Flush()
			<-req.Context().Done()
		})
		r.timeout = 40 * time.Millisecond
		_, err := r.Read(bg, url)
		is(t, []any{readErr(t, err).Kind, hits.Load()}, []any{ReadTimeout, int32(2)}, "body timeout retried")
	})

	t.Run("the caller's stop outranks dispatch and a late reply", func(t *testing.T) {
		var hits atomic.Int32
		r, url := tlsServed(t, func(w http.ResponseWriter, _ *http.Request) { hits.Add(1) })
		stopped, cancel := context.WithCancel(bg)
		cancel()
		_, err := r.Read(stopped, url)
		is(t, []any{readErr(t, err).Kind, hits.Load()}, []any{ReadCancelled, int32(0)}, "pre-stop zero dispatch")

		r2, url2 := tlsServed(t, func(w http.ResponseWriter, req *http.Request) {
			w.WriteHeader(200)
			_, _ = io.WriteString(w, `{"inventoryId":`)
			w.(http.Flusher).Flush()
			<-req.Context().Done()
		})
		ctx, cancel2 := context.WithCancel(bg)
		go func() { time.Sleep(50 * time.Millisecond); cancel2() }()
		_, err = r2.Read(ctx, url2)
		is(t, readErr(t, err).Kind, ReadCancelled, "cancel during body read")
	})

	t.Run("a 2xx read whole after a stop or the attempt bound is not accepted", func(t *testing.T) {
		const valid = `{"inventoryId":"i","registerExpectedMachines":false,"carbideApiUrl":"https://n","racks":[],"devices":[],"dpus":[]}`
		serve := func(code int, body io.ReadCloser) *HTTPReader {
			r := NewHTTPReader()
			r.client.Transport = roundTrip(func(*http.Request) (*http.Response, error) {
				return &http.Response{StatusCode: code, Body: body, Header: http.Header{}}, nil
			})
			return r
		}

		r := serve(200, &lateBody{data: []byte(valid), sleep: 60 * time.Millisecond})
		r.timeout = 20 * time.Millisecond
		_, re := r.readOnce(bg, "https://pod/records")
		is(t, re.Kind, ReadTimeout, "late whole reply") // whole reply after the attempt bound

		// Over the cap, but the caller cancelled during the read: cancel wins.
		ctx, cancel := context.WithCancel(bg)
		r = serve(200, nil)
		r.max = 10
		r.client.Transport = roundTrip(func(*http.Request) (*http.Response, error) {
			cancel()
			return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(strings.Repeat("x", 50))), Header: http.Header{}}, nil
		})
		_, re = r.readOnce(ctx, "https://pod/records")
		is(t, re.Kind, ReadCancelled, "cap+1 with caller cancel")

		var hits atomic.Int32 // a caller deadline passed but whose timer has not fired
		r2, url := tlsServed(t, func(http.ResponseWriter, *http.Request) { hits.Add(1) })
		_, err := r2.Read(delayedDeadline{bg, time.Now().Add(-time.Millisecond)}, url)
		is(t, []any{readErr(t, err).Kind, hits.Load()}, []any{ReadTimeout, int32(0)}, "delayed deadline")

		for _, tc := range []struct { // a stop surfacing during decode is not accepted
			name, body string
			cancel     bool
			want       ReadKind
		}{
			{"valid body, cancel during decode", valid, true, ReadCancelled},
			{"invalid body, deadline during decode", `{"racks":5}`, false, ReadTimeout},
		} {
			rr := serve(200, io.NopCloser(strings.NewReader(tc.body)))
			_, re := rr.readOnce(&stopAfterErr{Context: bg, after: 2, cancel: tc.cancel}, "https://pod/records")
			if re == nil || re.Kind != tc.want {
				t.Errorf("%s: got %v, want %v", tc.name, re, tc.want)
			}
		}
	})
}

// stopAfterErr becomes stopped on the (after+1)-th Err call, so a caller stop can be made to surface only after the pre-decode checks, i.e. during decode.
type stopAfterErr struct {
	context.Context
	n      atomic.Int32
	after  int32
	cancel bool
}

func (c *stopAfterErr) Err() error {
	if c.n.Add(1) > c.after {
		if c.cancel {
			return context.Canceled
		}
		return context.DeadlineExceeded
	}
	return nil
}

func (c *stopAfterErr) Deadline() (time.Time, bool) { return time.Time{}, false }
func (c *stopAfterErr) Done() <-chan struct{}       { return nil }

// lateBody gives its data on the first read, then sleeps past the attempt bound before EOF, so the whole reply arrives after the deadline.
type lateBody struct {
	data  []byte
	sleep time.Duration
	read  bool
}

func (b *lateBody) Read(p []byte) (int, error) {
	if !b.read {
		b.read = true
		return copy(p, b.data), nil
	}
	time.Sleep(b.sleep)
	return 0, io.EOF
}

func (b *lateBody) Close() error { return nil }

// delayedDeadline is a context whose deadline has passed but whose Err lags it.
type delayedDeadline struct {
	context.Context
	at time.Time
}

func (d delayedDeadline) Deadline() (time.Time, bool) { return d.at, true }
func (d delayedDeadline) Err() error                  { return nil }
