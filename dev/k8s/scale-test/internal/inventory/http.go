// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventory

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

const (
	readTimeout = 10 * time.Second
	readTries   = 2
	// maxRecordsBytes is the largest records reply read from one pod. A pod serving 2,000 devices answers well under 4 MiB.
	maxRecordsBytes = 64 << 20
)

// ReadKind tells a read failure apart: a stop (the caller cancelled or its deadline passed) versus a transport, status or decode fault.
type ReadKind string

const (
	ReadTimeout   ReadKind = "timeout"
	ReadCancelled ReadKind = "cancelled"
	ReadTransport ReadKind = "transport"
	ReadStatus    ReadKind = "status"
	ReadOversize  ReadKind = "oversize"
	ReadDecode    ReadKind = "decode"
)

// ReadError is a failed records read. Detail is fixed text: it never holds the URL or any reply content. transient is whether another try may pass.
type ReadError struct {
	Kind      ReadKind
	Detail    string
	transient bool
}

func (e *ReadError) Error() string { return e.Detail }

// HTTPReader reads the records route over HTTPS. MAT serves a self-signed certificate, so any is accepted. Each read gets two 10 s tries, the second only after a
// transient failure; a redirect is not followed and a reply is read to at most 64 MiB. A failure names the status, the decode-error kind or the transport cause, never
// reply content (MAT config) or the URL.
type HTTPReader struct {
	client  *http.Client
	max     int
	timeout time.Duration
}

// NewHTTPReader builds the reader. The transport accepts any certificate, follows no redirect, takes no proxy and keeps no connection, so net/http never replays an
// attempt on a dead connection.
func NewHTTPReader() *HTTPReader {
	only1 := new(http.Protocols)
	only1.SetHTTP1(true)
	client := &http.Client{
		Transport: &http.Transport{
			DisableKeepAlives: true,
			Proxy:             nil,
			Protocols:         only1,
			// #nosec G402 -- MAT serves a self-signed cert on its pod IP; the
			// records hold no secret and the URL is a fixed in-cluster pod IP.
			TLSClientConfig: &tls.Config{InsecureSkipVerify: true},
		},
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
	return &HTTPReader{client, maxRecordsBytes, readTimeout}
}

// Read tries the route up to readTries times, the second only after a transient failure. The caller's cancellation and its elapsed deadline outrank every other outcome
// and stop the retries.
func (r *HTTPReader) Read(ctx context.Context, url string) (Response, error) {
	var last *ReadError
	for range readTries {
		out, re := r.readOnce(ctx, url)
		if re == nil {
			return out, nil
		}
		last = re
		if !re.transient {
			break
		}
	}
	return Response{}, last
}

// readOnce is one try. The caller's stop is checked before dispatch, on a failure and before a decoded reply is accepted, so a late success never passes. A non-success
// status fails before its body; only 408, 429 and 5xx are worth another try.
func (r *HTTPReader) readOnce(caller context.Context, url string) (Response, *ReadError) {
	if e := callerStop(caller); e != nil {
		return Response{}, e
	}
	ctx, cancel := context.WithTimeout(caller, r.timeout)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return Response{}, &ReadError{ReadTransport, "the request failed", false}
	}
	resp, err := r.client.Do(req)
	if err != nil {
		return Response{}, r.transportError(caller, ctx, err, "the request failed")
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		code := resp.StatusCode
		transient := code >= 500 && code < 600 || code == http.StatusTooManyRequests || code == http.StatusRequestTimeout
		return Response{}, &ReadError{ReadStatus, fmt.Sprintf("status %d", code), transient}
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, int64(r.max)+1))
	if err != nil && len(data) <= r.max {
		return Response{}, r.transportError(caller, ctx, err, "the reply body failed")
	}
	// The body is whole (or over the cap). A caller stop or the elapsed attempt bound outranks the oversize, decode and success verdicts: a reply read whole after the
	// caller gave up is not a reply.
	if re := r.stopped(caller, ctx); re != nil {
		return Response{}, re
	}
	if len(data) > r.max {
		return Response{}, &ReadError{ReadOversize, fmt.Sprintf("the reply is over %d bytes", r.max), false}
	}
	var out Response
	decodeErr := jsonx.Decode(data, &out)
	// Decode can be slow, so the stop and the attempt bound are rechecked after it: a reply decoded past either is not a reply, decoded or not.
	if re := r.stopped(caller, ctx); re != nil {
		return Response{}, re
	}
	if decodeErr != nil {
		return Response{}, &ReadError{ReadDecode, decodeErr.Error(), false} // jsonx text holds no input
	}
	return out, nil
}

// stopped is the caller's stop (final), else the elapsed attempt bound (transient), that outranks a 2xx verdict, or nil.
func (r *HTTPReader) stopped(caller, attempt context.Context) *ReadError {
	if s := callerStop(caller); s != nil {
		return s
	}
	if deadlinePassed(attempt) {
		return &ReadError{ReadTimeout, "timed out", true}
	}
	return nil
}

// transportError is a call with no whole reply. The caller's stop outranks it; else a per-attempt timeout is a timeout and a dial failure a connect failure. It never
// wraps the library error, which holds the URL.
func (r *HTTPReader) transportError(caller, attempt context.Context, err error, failed string) *ReadError {
	if e := callerStop(caller); e != nil {
		return e
	}
	var op *net.OpError
	switch {
	case errors.Is(attempt.Err(), context.DeadlineExceeded) || deadlinePassed(attempt):
		return &ReadError{ReadTimeout, "timed out", true}
	case errors.As(err, &op) && op.Op == "dial":
		return &ReadError{ReadTransport, "could not connect", true}
	}
	return &ReadError{ReadTransport, failed, true}
}

// callerStop is the caller's cancellation or elapsed deadline as a safe local error, or nil. The deadline is read directly, so an expiry whose timer has not fired
// counts. A deadline is a timeout, any other cancel a cancel; neither is transient, as the caller has given up.
func callerStop(ctx context.Context) *ReadError {
	switch err := ctx.Err(); {
	case err == nil:
		if deadlinePassed(ctx) {
			return &ReadError{ReadTimeout, "timed out", false}
		}
		return nil
	case errors.Is(err, context.DeadlineExceeded):
		return &ReadError{ReadTimeout, "timed out", false}
	default:
		return &ReadError{ReadCancelled, "cancelled", false}
	}
}

func deadlinePassed(ctx context.Context) bool {
	d, ok := ctx.Deadline()
	return ok && !time.Now().Before(d)
}
