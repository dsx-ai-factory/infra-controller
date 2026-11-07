// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package probe

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

const (
	maxBody       = 16 << 20 // a full page of units is far below it
	maxErrorBody  = 64 << 10 // a probe error is one small object
	maxRetryAfter = 60       // seconds
)

type httpProbe struct {
	client  *http.Client
	base    *url.URL
	timeout time.Duration
}

// NewHTTP is a Probe over HTTP at base, each call bounded by timeout. The
// URL rules are the config's, so a client built by hand holds no userinfo,
// path, query or fragment, and a non-positive timeout is refused.
func NewHTTP(base string, timeout time.Duration) (Probe, error) {
	bad := func(detail string) (Probe, error) { return nil, &Error{Kind: InvalidParam, Detail: detail} }
	if timeout <= 0 {
		return bad("the probe timeout must be over zero")
	}
	u, err := url.Parse(base)
	if err == nil && u.Port() != "" {
		_, err = strconv.ParseUint(u.Port(), 10, 16)
	}
	switch {
	case err != nil || u.Scheme != "http" && u.Scheme != "https" || u.Hostname() == "":
		return bad("the probe URL is not http or https with a host")
	case u.User != nil:
		return bad("the probe URL must hold no user name or password")
	case strings.ContainsAny(base, "?#"):
		return bad("the probe URL must hold no query or fragment")
	case u.Path != "" && u.Path != "/":
		return bad("the probe URL must hold no path")
	}
	// The probe is a fixed in-cluster Service: follow no redirect and take no
	// proxy from the environment. Keep-alives are off so net/http never
	// silently replays an idempotent request on a reused-but-dead connection,
	// and the client is pinned to HTTP/1.1 so the HTTP/2 transport cannot
	// retry a REFUSED_STREAM either; both would break one attempt per call.
	only1 := new(http.Protocols)
	only1.SetHTTP1(true)
	client := &http.Client{
		Transport:     &http.Transport{DisableKeepAlives: true, Protocols: only1},
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
	return &httpProbe{client, u, timeout}, nil
}

func (p *httpProbe) Count(ctx context.Context, f probeapi.Filter) (probeapi.CountResponse, error) {
	q, err := f.Query()
	return send[probeapi.CountResponse](ctx, p, http.MethodGet, "/v1/count", q, nil, err)
}

func (p *httpProbe) Units(ctx context.Context, f probeapi.Filter, name *string, pg Page) (probeapi.UnitsResponse, error) {
	q, err := f.Query()
	if err == nil {
		set(q, "name", name)
		set(q, "cursor", pg.Cursor)
		if pg.Limit != nil {
			q.Set("limit", strconv.FormatUint(uint64(*pg.Limit), 10))
		}
	}
	return send[probeapi.UnitsResponse](ctx, p, http.MethodGet, "/v1/units", q, nil, err)
}

func (p *httpProbe) UnitsQuery(ctx context.Context, uq probeapi.UnitsQuery) (probeapi.UnitsResponse, error) {
	body, err := jsonx.Encode(uq)
	return send[probeapi.UnitsResponse](ctx, p, http.MethodPost, "/v1/units/query", nil, body, err)
}

func (p *httpProbe) Config(ctx context.Context, f ConfigFilter) (probeapi.ConfigResponse, error) {
	q := url.Values{}
	set(q, "vendor", f.Vendor)
	set(q, "model", f.Model)
	set(q, "component", (*string)(f.Component))
	set(q, "instanceType", f.InstanceType)
	return send[probeapi.ConfigResponse](ctx, p, http.MethodGet, "/v1/config", q, nil, nil)
}

func set(q url.Values, key string, v *string) {
	if v != nil {
		q.Set(key, *v)
	}
}

// send makes one attempt. Only a 2xx is a reply. Bodies are read up to their
// cap within the timeout, and no error holds the URL or reply text. encErr
// is a request this client cannot encode: final, never sent.
func send[T any](ctx context.Context, p *httpProbe, method, path string, q url.Values, body []byte, encErr error) (T, error) {
	var out T
	ctx, cancel := context.WithTimeout(ctx, p.timeout)
	defer cancel()
	u := p.base.ResolveReference(&url.URL{Path: path, RawQuery: q.Encode()})
	req, err := http.NewRequestWithContext(ctx, method, u.String(), bytes.NewReader(body))
	if err = errors.Join(encErr, err); err != nil {
		return out, &Error{Kind: InvalidParam, Detail: "the request could not be built"}
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := p.client.Do(req)
	if err != nil {
		if stop := stopError(ctx); stop != nil { // a transport error past a stop is the stop
			return out, stop
		}
		return out, failed(ctx, err, "request failed")
	}
	defer func() { _ = resp.Body.Close() }()
	// An upgraded stream: no cancel ends a read of it.
	if resp.StatusCode == http.StatusSwitchingProtocols {
		return out, fromReply(resp.StatusCode, 0, nil)
	}
	ok, limit := resp.StatusCode >= 200 && resp.StatusCode < 300, int64(maxErrorBody)
	if ok {
		limit = maxBody
	}
	tooBig := &Error{Kind: Decode, Detail: fmt.Sprintf("reply body is over %d bytes", limit)}
	// A cancel or an elapsed deadline outranks a size or decode verdict on a
	// 2xx: an oversized reply read after a stop is the stop, not a Decode.
	// A non-2xx keeps its status precedence, so this guards 2xx exits only.
	if ok && resp.ContentLength > limit {
		if stop := stopError(ctx); stop != nil {
			return out, stop
		}
		return out, tooBig
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, limit+1))
	over := int64(len(data)) > limit
	switch {
	case !ok:
		// The status already says what a refusal is. A detail is trusted
		// only from a body that ended within its cap.
		if err != nil || over {
			data = nil
		}
		return out, fromReply(resp.StatusCode, retryAfter(resp.Header.Get("Retry-After")), data)
	case err != nil:
		if stop := stopError(ctx); stop != nil { // a read that failed past a stop is the stop
			return out, stop
		}
		return out, failed(ctx, err, "reply body failed")
	case over:
		if stop := stopError(ctx); stop != nil {
			return out, stop
		}
		return out, tooBig
	}
	err = jsonx.Decode(data, &out)
	if stop := stopError(ctx); stop != nil { // a reply read whole after a stop is not a reply
		return out, stop
	}
	if err != nil {
		return out, &Error{Kind: Decode, Detail: err.Error()} // a path and a reason, never input
	}
	return out, nil
}

// stopError is the Cancelled or Timeout that ended a 2xx read, or nil. A
// cancel or an elapsed deadline (even before the context's timer has run)
// outranks any other verdict.
func stopError(ctx context.Context) *Error {
	deadline, ok := ctx.Deadline()
	switch {
	case ctx.Err() != nil:
		return failed(ctx, nil, "")
	case ok && !time.Now().Before(deadline):
		return &Error{Kind: Timeout}
	}
	return nil
}

// failed is a call with no whole reply. It keeps only the kind, never the
// library's text, which holds the URL.
func failed(ctx context.Context, err error, kind string) *Error {
	var op *net.OpError
	switch cause := context.Cause(ctx); {
	case errors.Is(cause, context.DeadlineExceeded), errors.Is(cause, errDeadline):
		return &Error{Kind: Timeout}
	case cause != nil:
		return &Error{Kind: Cancelled}
	case errors.As(err, &op) && op.Op == "dial":
		kind = "connection failed"
	}
	return &Error{Kind: Transport, Detail: kind}
}

// retryAfter is Retry-After in whole seconds, 1 to 60 s. The date form is
// not used by the probe.
func retryAfter(v string) time.Duration {
	s, err := strconv.ParseUint(strings.TrimSpace(v), 10, 64)
	if err != nil {
		return 0
	}
	return time.Duration(min(max(s, 1), maxRetryAfter)) * time.Second
}
