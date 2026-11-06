// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"io"
	"maps"
	"net/http"
	"net/url"
	"os"
	"slices"
	"strings"
	"syscall"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/history"
)

const (
	// MaxIngestionDelay is the longest ingestion delay: a store that hides
	// samples longer cannot show a sweep end in time for a run to wait for it.
	MaxIngestionDelay = 60 * time.Second
	// caFileCap holds a CA bundle of a few certificates.
	caFileCap = 1 << 20
	// tokenFileCap holds a bearer token, a service account JWT included.
	tokenFileCap = 64 << 10
	// maxHeaders is a tenant header or two, with room.
	maxHeaders = 16
	// tokenRefresh is how long a token read from its file is used before
	// it is read again, so a rotated token is used.
	tokenRefresh = time.Minute
)

// Config is where and how to reach a Prometheus-compatible store. Secrets come
// from mounted files only, never the config itself, so they stay out of args,
// config maps and logs. Errors name what is wrong, never content.
type Config struct {
	// URL is the query base URL, its path prefix included: /api/v1/query is
	// appended. http://prometheus:9090, or a /select/0/prometheus prefix for a
	// VictoriaMetrics cluster tenant.
	URL string `json:"url" yaml:"url"`
	// BearerTokenFile is a mounted file with the bearer token, read again
	// at most every minute.
	BearerTokenFile *string `json:"bearerTokenFile" yaml:"bearerTokenFile"`
	// CAFile is a mounted PEM file of the CAs that sign the store's
	// certificate. When set, only these CAs are trusted.
	CAFile *string `json:"caFile" yaml:"caFile"`
	// Headers go with every query, such as a tenant: X-Scope-OrgID for
	// Mimir. Never Authorization: a token comes from BearerTokenFile.
	Headers map[string]string `json:"headers,omitempty" yaml:"headers"`
	// IngestionDelaySeconds is the longest time from a scrape's timestamp until
	// every sample is queryable. 0 for a Prometheus that scrapes the probe itself.
	IngestionDelaySeconds uint64 `json:"ingestionDelaySeconds,omitempty" yaml:"ingestionDelaySeconds"`
}

func (Config) JSONClosed() {}

// String shows the URL without credentials, query or fragment, the header
// names, and whether a token or CA file is set. A URL that is opaque, not
// http(s) or hostless has no parts to strip safely, so it shows a placeholder.
func (c Config) String() string {
	shown := "<not a URL>"
	if u, err := url.Parse(c.URL); err == nil && u.Opaque == "" && (u.Scheme == "http" || u.Scheme == "https") && u.Hostname() != "" {
		u.User, u.RawQuery, u.Fragment = nil, "", ""
		shown = u.String()
	}
	return fmt.Sprintf("store.Config{url: %s, bearerTokenFile: %t, caFile: %t, headers: %v, ingestionDelaySeconds: %d}",
		shown, c.BearerTokenFile != nil, c.CAFile != nil, slices.Sorted(maps.Keys(c.Headers)), c.IngestionDelaySeconds)
}

func unsupported(what string) error { return history.Unsupported{Text: "the metrics store " + what} }

// transport checks the headers and reads the CA file once, so a store that
// can never be queried is refused before a run.
func (c Config) transport() (http.Header, *http.Transport, error) {
	if c.IngestionDelaySeconds > uint64(MaxIngestionDelay/time.Second) {
		return nil, nil, unsupported(fmt.Sprintf("ingestion delay is over %ds", MaxIngestionDelay/time.Second))
	}
	if len(c.Headers) > maxHeaders {
		return nil, nil, unsupported(fmt.Sprintf("takes at most %d headers", maxHeaders))
	}
	headers := http.Header{}
	for name, value := range c.Headers {
		switch {
		case name == "" || strings.ContainsFunc(name, func(r rune) bool {
			return r > '~' || r <= ' ' || strings.ContainsRune(`"(),/:;<=>?@[\]{}`, r)
		}):
			return nil, nil, unsupported("header name is not valid")
		case strings.EqualFold(name, "Authorization"):
			return nil, nil, unsupported("Authorization header comes from bearerTokenFile")
		case strings.ContainsFunc(value, func(r rune) bool { return r < ' ' && r != '\t' || r == 0x7f }):
			return nil, nil, unsupported("header " + strings.ToLower(name) + " is not valid")
		}
		headers.Set(name, value)
	}
	// An owned transport, HTTP/1.1 only with keep-alives off, not a clone of the
	// default: net/http never replays a lost request on a reused dead connection
	// and HTTP/2 cannot retry a REFUSED_STREAM, so one dispatch per call.
	only1 := new(http.Protocols)
	only1.SetHTTP1(true)
	t := &http.Transport{DisableKeepAlives: true, Protocols: only1}
	if c.CAFile != nil {
		pem, ok := readCapped(*c.CAFile, caFileCap)
		if !ok {
			return nil, nil, unsupported("CA file cannot be read")
		}
		roots := x509.NewCertPool()
		if !roots.AppendCertsFromPEM(pem) {
			return nil, nil, unsupported("CA file holds no PEM certificate")
		}
		t.TLSClientConfig = &tls.Config{RootCAs: roots, MinVersion: tls.VersionTLS12}
	}
	return headers, t, nil
}

// readCapped is at most limit bytes of the regular file path, false when it
// cannot be read or is longer. It is opened non-blocking and checked as opened,
// so a FIFO or device in its place is refused, never waited on.
func readCapped(path string, limit int64) ([]byte, bool) {
	f, err := os.OpenFile(path, os.O_RDONLY|syscall.O_NONBLOCK, 0)
	if err != nil {
		return nil, false
	}
	defer func() { _ = f.Close() }()
	if info, err := f.Stat(); err != nil || !info.Mode().IsRegular() {
		return nil, false
	}
	content, err := io.ReadAll(io.LimitReader(f, limit+1))
	return content, err == nil && int64(len(content)) <= limit
}

// bearer is the Authorization value of the token in path, or what is wrong with
// the file. A token is visible ASCII; surrounding whitespace is dropped.
func bearer(path string) (string, error) {
	content, ok := readCapped(path, tokenFileCap)
	if !ok {
		return "", unsupported("bearer token file cannot be read")
	}
	token := bytes.TrimSpace(content)
	if len(token) == 0 || bytes.ContainsFunc(token, func(r rune) bool { return r <= ' ' || r > '~' }) {
		return "", unsupported("bearer token file holds no token")
	}
	return "Bearer " + string(token), nil
}

// tokenFile is the bearer token of a mounted file, read again at most every
// tokenRefresh. One read runs at a time, bounded by the caller's context and at
// most timeout: a read stuck on a stalled mount keeps one goroutine and every
// query meanwhile fails at once.
type tokenFile struct {
	timeout time.Duration
	lock    chan struct{}
	token   string
	readAt  time.Time
	reading chan tokenRead
	// read loads the token from the file; a test swaps in a blocking read.
	read func() tokenRead
}

type tokenRead struct {
	token string
	err   error
}

func (t *tokenFile) current(ctx context.Context) (string, error) {
	unreadable := unsupported("bearer token file cannot be read")
	lockTimer := time.NewTimer(t.timeout)
	defer lockTimer.Stop()
	select {
	case t.lock <- struct{}{}:
		defer func() { <-t.lock }()
	case <-ctx.Done():
		return "", ctx.Err()
	case <-lockTimer.C:
		return "", unreadable
	}
	if time.Since(t.readAt) < tokenRefresh {
		return t.token, nil
	}
	take := func(read tokenRead) (string, error) {
		t.reading = nil
		if read.err != nil {
			return "", read.err
		}
		t.token, t.readAt = read.token, time.Now()
		return t.token, nil
	}
	if t.reading != nil {
		select {
		case read := <-t.reading:
			return take(read)
		default:
			// A read another caller started is still running: a cancelled or
			// expired caller gets its own error, anyone else fails at once.
			if err := ctx.Err(); err != nil {
				return "", err
			}
			return "", unreadable
		}
	}
	t.reading = make(chan tokenRead, 1)
	go func(done chan<- tokenRead) { done <- t.read() }(t.reading)
	timer := time.NewTimer(t.timeout)
	defer timer.Stop()
	select {
	case read := <-t.reading:
		return take(read)
	case <-ctx.Done():
		return "", ctx.Err()
	case <-timer.C:
		return "", unreadable
	}
}
