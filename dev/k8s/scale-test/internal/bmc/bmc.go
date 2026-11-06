// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package bmc is the fleet-up BMC gate: every expected BMC must answer its Redfish service root over HTTPS before the
// readiness deadline.
package bmc

import (
	"cmp"
	"context"
	"crypto/tls"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/netip"
	"slices"
	"strconv"
	"sync"
	"time"
)

// maxReply is the largest service root reply read; a real one is a few KiB.
const maxReply = 64 << 10

var errRedirect = errors.New("redirect refused")

// wrap is the gate's transport as sent; a test wraps it.
var wrap = func(t *http.Transport) http.RoundTripper { return t }

// Settings are the gate's knobs. A zero field takes its default: port 443, 64 BMCs at once, 5 s per try, 2 tries.
type Settings struct {
	Port           uint16 `yaml:"port"`
	Concurrency    uint16 `yaml:"concurrency"`
	TimeoutSeconds uint16 `yaml:"timeoutSeconds"`
	Tries          uint8  `yaml:"tries"`
}

// WithDefaults fills each zero field with its default.
func (s Settings) WithDefaults() Settings {
	s.Port = cmp.Or(s.Port, 443)
	s.Concurrency = cmp.Or(s.Concurrency, 64)
	s.TimeoutSeconds = cmp.Or(s.TimeoutSeconds, 5)
	s.Tries = cmp.Or(s.Tries, 2)
	return s
}

// Failure is a BMC that did not pass. Detail is fixed text, a status code at most: never the address or reply text.
// Unchecked is a BMC the deadline or a cancel stopped before it finished its tries.
type Failure struct {
	IP        netip.Addr
	Detail    string
	Unchecked bool
}

// Dialer is the host:port a BMC's IP is reached at.
type Dialer interface{ Address(ip netip.Addr) string }

// DirectDialer reaches each BMC at its own IP on Port.
type DirectDialer struct{ Port uint16 }

func (d DirectDialer) Address(ip netip.Addr) string { return netip.AddrPortFrom(ip, d.Port).String() }

// Check tries each BMC up to Tries times, at most Concurrency at once, every try bounded by its timeout and the
// deadline. A BMC passes on a 200 whose JSON body is a Redfish service root. It returns the failures sorted by IP.
func Check(ctx context.Context, ips []netip.Addr, d Dialer, s Settings, deadline time.Time) []Failure {
	s = s.WithDefaults()
	ctx, cancel := context.WithDeadline(ctx, deadline)
	defer cancel()
	only1 := new(http.Protocols)
	only1.SetHTTP1(true)
	// One connection per try: no reuse, no proxy, no redirect, HTTP/1.1 only, so net/http never replays a try.
	client := &http.Client{
		Transport: wrap(&http.Transport{DisableKeepAlives: true, Protocols: only1,
			// Simulated and real BMCs serve self-signed certificates, and the gate checks only reachability and the
			// Redfish service root. It sends no credential, so skipping verification exposes nothing. If the gate ever
			// sends a credential, it must verify the certificate instead.
			// #nosec G402 -- no credential is sent, see above.
			TLSClientConfig: &tls.Config{InsecureSkipVerify: true, MinVersion: tls.VersionTLS12}}),
		CheckRedirect: func(*http.Request, []*http.Request) error { return errRedirect },
	}
	found := make([]*Failure, len(ips))
	jobs := make(chan int)
	var wg sync.WaitGroup
	for range min(int(s.Concurrency), len(ips)) {
		wg.Go(func() {
			for i := range jobs {
				found[i] = checkOne(ctx, client, d, s, ips[i])
			}
		})
	}
	for i := range ips {
		jobs <- i
	}
	close(jobs)
	wg.Wait()
	var out []Failure
	for _, f := range found {
		if f != nil {
			out = append(out, *f)
		}
	}
	slices.SortFunc(out, func(a, b Failure) int { return a.IP.Compare(b.IP) })
	return out
}

func checkOne(ctx context.Context, c *http.Client, d Dialer, s Settings, ip netip.Addr) *Failure {
	url, host := "https://"+d.Address(ip)+"/redfish/v1", ip.String()
	if ip.Is6() {
		host = "[" + host + "]"
	}
	// A cancel, then the deadline, stops the BMC unchecked: before each try, and after the last, so a stop during the
	// final try is no checked failure.
	unchecked := func() *Failure {
		switch {
		case ctx.Err() == context.Canceled:
			return &Failure{IP: ip, Detail: "not checked: cancelled", Unchecked: true}
		case stopped(ctx):
			return &Failure{IP: ip, Detail: "not checked before the readiness deadline", Unchecked: true}
		}
		return nil
	}
	var detail string
	for range s.Tries {
		if f := unchecked(); f != nil {
			return f
		}
		var retry bool
		if detail, retry = try(ctx, c, url, host, time.Duration(s.TimeoutSeconds)*time.Second); detail == "" || !retry {
			break
		}
	}
	if f := unchecked(); f != nil {
		return f
	}
	if detail == "" {
		return nil
	}
	return &Failure{IP: ip, Detail: detail}
}

// try is one GET of the service root with the Host the BMC is routed by. It returns "" on a pass, else a fixed detail
// and whether another try may pass: a transport fault, a cut reply, 408, 429 or 5xx. A root read once the try is
// stopped is not taken.
func try(ctx context.Context, c *http.Client, url, host string, timeout time.Duration) (string, bool) {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return "the request did not build", false
	}
	req.Host = host
	resp, err := c.Do(req)
	switch {
	case errors.Is(err, errRedirect):
		return "redirect refused", false
	case err != nil && ctx.Err() != nil:
		return "no reply before the timeout", true
	case err != nil:
		return "unreachable", true
	}
	defer func() { _ = resp.Body.Close() }()
	if code := resp.StatusCode; code != http.StatusOK {
		return "status " + strconv.Itoa(code), code == 408 || code == 429 || code >= 500
	}
	body, err := io.ReadAll(io.LimitReader(resp.Body, maxReply+1))
	var root struct {
		ID string `json:"@odata.id"`
	}
	switch {
	case err != nil:
		return "the reply was cut short", true
	case len(body) > maxReply:
		return "reply over 64 KiB", false
	case json.Unmarshal(body, &root) != nil || root.ID != "/redfish/v1" && root.ID != "/redfish/v1/":
		return "not a Redfish service root", false
	case stopped(ctx):
		return "no reply before the timeout", true
	}
	return "", false
}

// stopped is whether ctx is done or its deadline has passed, which can be before its Done fires.
func stopped(ctx context.Context) bool {
	deadline, ok := ctx.Deadline()
	return ctx.Err() != nil || ok && !time.Now().Before(deadline)
}
