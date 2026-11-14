// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"bytes"
	"context"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"syscall"
	"time"

	"github.com/google/uuid"
	"golang.org/x/time/rate"
	"gopkg.in/yaml.v3"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/fleetup"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

// InstanceSettings extends the existing runner fixture. Secrets are mounted files.
type InstanceSettings struct {
	Fixture       FixtureSettings   `yaml:"fixture"`
	sidecar       fixtureProbe      `yaml:"-"`
	restEpoch     float64           `yaml:"-"`
	lifecycleBase *instanceBaseline `yaml:"-"`
	journalRunID  string            `yaml:"-"`
	tenantCensus  bool              `yaml:"-"`
	preparing     bool              `yaml:"-"`

	fleetup.Settings `yaml:",inline"`
	Rest             struct {
		URL         string `yaml:"url"`
		Org         string `yaml:"org"`
		ProviderOrg string `yaml:"providerOrg"`
		TokenFile   string `yaml:"tokenFile"`
		CAFile      string `yaml:"caFile"`

		Auth fixtureAuth `yaml:"auth"`
	} `yaml:"rest"`
	Observation struct {
		IntervalSeconds      uint64 `yaml:"intervalSeconds"`
		LabelReadbackSeconds uint64 `yaml:"labelReadbackSeconds"`
	} `yaml:"observation"`
	Instance struct {
		TopologyOptimized bool              `yaml:"topologyOptimized"`
		N                 uint64            `yaml:"n"`
		BurstSize         uint64            `yaml:"burstSize"`
		TenantID          string            `yaml:"tenantId"`
		InstanceTypeID    string            `yaml:"instanceTypeId"`
		VPCID             string            `yaml:"vpcId"`
		SubnetID          string            `yaml:"subnetId"`
		IPXEFile          string            `yaml:"ipxeFile"`
		OwnedManifest     string            `yaml:"ownedManifest"`
		APIPoolReplicas   uint64            `yaml:"apiPoolReplicas"`
		APIPoolSelector   map[string]string `yaml:"apiPoolSelector"`

		OperatingSystemID string `yaml:"operatingSystemId"`
		DeleteCount       uint64 `yaml:"deleteCount"`
	} `yaml:"instance"`
}

// ParseInstanceSettings validates a create/delete fixture before any cluster work.
func ParseInstanceSettings(n yaml.Node, command string) (InstanceSettings, error) {
	s := InstanceSettings{Settings: fleetup.DefaultSettings()}
	s.Instance.BurstSize = 100
	s.Observation.LabelReadbackSeconds = 1800
	if err := fleetup.DecodeSettings(n, &s); err != nil {
		return s, err
	}
	u, err := url.Parse(s.Rest.URL)
	if err != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || strings.ContainsAny(s.Rest.URL, "?#") || u.Path != "" && u.Path != "/" {
		return s, instanceIssue("REST needs an HTTPS origin without credentials, query or path")
	}
	if s.Rest.Org == "" || s.Rest.ProviderOrg == "" || s.Rest.Org == s.Rest.ProviderOrg || s.Rest.TokenFile == "" && s.Rest.Auth.TokenURL == "" || !s.CheckStore || len(s.Instance.APIPoolSelector) == 0 || s.Instance.APIPoolReplicas == 0 {
		return s, instanceIssue("instance fixture needs distinct tenant/provider orgs, a token file and API-pool store selector")
	}
	if s.Observation.LabelReadbackSeconds < 30 || s.Observation.LabelReadbackSeconds > 86400 {
		return s, instanceIssue("label read-back window must be 30 to 86400 seconds")
	}
	if s.Observation.IntervalSeconds < 30 || s.Observation.IntervalSeconds > 86400 || s.Instance.N == 0 || s.Instance.BurstSize < 2 {
		return s, instanceIssue("instance fixture needs positive N, bursts of at least two and a 30 to 86400 second cadence")
	}
	if command != "instance-delete" && s.Instance.DeleteCount != 0 {
		return s, instanceIssue("deleteCount is only valid for instance-delete")
	}
	if s.Fixture.Name != "" {
		if command != "instance-delete" && (s.Instance.N < 2 || s.Instance.IPXEFile == "") || command == "instance-delete" && s.Instance.OwnedManifest == "" {
			return s, instanceIssue("fixture command needs create iPXE or an owned delete manifest")
		}
		return s, validateFixture(s)
	}
	if s.Rest.Auth != (fixtureAuth{}) {
		return s, instanceIssue("credential auth requires fixture mode")
	}
	for _, id := range []string{s.Instance.TenantID, s.Instance.InstanceTypeID} {
		if uuid.Validate(id) != nil {
			return s, instanceIssue("tenant and instance type must be UUIDs")
		}
	}
	if command != "instance-delete" {
		if s.Instance.N < 2 || uuid.Validate(s.Instance.VPCID) != nil || uuid.Validate(s.Instance.SubnetID) != nil || s.Instance.IPXEFile == "" {
			return s, instanceIssue("create needs at least two hosts, VPC/subnet UUIDs and a pinned iPXE file")
		}
	} else if s.Instance.OwnedManifest == "" {
		return s, instanceIssue("delete needs an owned manifest")
	}
	return s, nil
}

// restWrites has no read method. State and read-back come only from the probe.
type restWrites struct {
	client   *http.Client
	settings InstanceSettings
	pace     *rate.Limiter
	until    time.Time
	token    string

	auth       restAuth
	authClient *http.Client
}

func newREST(s InstanceSettings) (*restWrites, error) {
	protocols := new(http.Protocols)
	protocols.SetHTTP1(true)
	transport := &http.Transport{DisableKeepAlives: true, Protocols: protocols}
	var identityRoots *x509.CertPool
	if s.Rest.CAFile != "" {
		f, err := os.OpenFile(s.Rest.CAFile, os.O_RDONLY|syscall.O_NONBLOCK, 0)
		if err != nil {
			return nil, instanceIssue("REST CA file cannot be read")
		}
		defer func() { _ = f.Close() }()
		info, err := f.Stat()
		if err != nil || !info.Mode().IsRegular() {
			return nil, instanceIssue("REST CA file must be regular")
		}
		pem, err := io.ReadAll(io.LimitReader(f, (1<<20)+1))
		if err != nil || len(pem) > 1<<20 {
			return nil, instanceIssue("REST CA file cannot be read within 1 MiB")
		}
		roots := x509.NewCertPool()
		if !roots.AppendCertsFromPEM(pem) {
			return nil, instanceIssue("REST CA file holds no PEM certificate")
		}
		transport.TLSClientConfig = &tls.Config{RootCAs: roots, MinVersion: tls.VersionTLS12}
		if s.Rest.Auth.TokenURL != "" {
			identityRoots, _ = x509.SystemCertPool()
			if identityRoots == nil {
				identityRoots = x509.NewCertPool()
			}
			identityRoots.AppendCertsFromPEM(pem)
		}
	}
	r := &restWrites{client: &http.Client{Transport: transport, Timeout: time.Minute, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}, settings: s, pace: rate.NewLimiter(2, 1)}
	if s.Fixture.Name != "" {
		r.client.Timeout = 300 * time.Second
	}
	if s.Rest.Auth.TokenURL != "" {
		identityTransport := transport.Clone()
		identityTransport.TLSClientConfig = &tls.Config{RootCAs: identityRoots, MinVersion: tls.VersionTLS12}
		client := *r.client
		client.Transport = identityTransport
		r.authClient = &client
		return r, nil
	}
	return r, r.refreshToken()
}
func (r *restWrites) refreshToken() error {
	info, err := os.Stat(r.settings.Rest.TokenFile)
	if err != nil || !info.Mode().IsRegular() {
		return instanceIssue("REST token file is unavailable")
	}
	f, err := os.Open(r.settings.Rest.TokenFile)
	if err != nil {
		return instanceIssue("REST token file could not be opened")
	}
	defer func() { _ = f.Close() }()
	b, err := io.ReadAll(io.LimitReader(f, 16385))
	token := strings.TrimSpace(string(b))
	if err != nil || len(b) > 16384 || token == "" || strings.ContainsFunc(token, func(r rune) bool { return r < 33 || r > 126 }) {
		return instanceIssue("REST token file is empty, invalid or over its bound")
	}
	r.token = token
	return nil
}

func digest(body []byte) string { h := sha256.Sum256(body); return hex.EncodeToString(h[:]) }

// attempt is one bounded mutation. Even a malformed 2xx is unclear.
func (r *restWrites) attempt(ctx context.Context, method, path string, body []byte, count int, provider ...bool) writer.Attempt {
	lost := writer.Attempt{NoReply: true}
	org := r.settings.Rest.Org
	if method == http.MethodPatch && len(provider) == 0 || len(provider) > 0 && provider[0] {
		org = r.settings.Rest.ProviderOrg
	}
	target := strings.TrimRight(r.settings.Rest.URL, "/") + "/v2/org/" + url.PathEscape(org) + "/nico/" + path
	request, err := http.NewRequestWithContext(ctx, method, target, bytes.NewReader(body))
	if err != nil {
		return lost
	}
	request.Header.Set("Authorization", "Bearer "+r.token)
	request.Header.Set("Content-Type", "application/json")
	reply, err := r.client.Do(request)
	if err != nil {
		return lost
	}
	defer func() { _ = reply.Body.Close() }()
	if reply.StatusCode < 200 || reply.StatusCode >= 300 {
		reason := journal.Reason("http-rejected")
		if reply.StatusCode >= 500 && r.settings.Fixture.Name != "" {
			return lost
		}
		if reply.StatusCode >= 500 {
			reason = journal.ServerError
		}
		if reply.StatusCode == 409 {
			reason = journal.Conflict
		}
		if reply.StatusCode == 429 {
			reason = journal.Timeout
			raw, err := io.ReadAll(io.LimitReader(reply.Body, 4097))
			var refusal struct {
				Message string  `json:"message"`
				Data    *string `json:"data"`
			}
			var fields map[string]json.RawMessage // Require explicit null, not an omitted nullable field.
			// Only the pre-handler limiter emits this exact response with null data.
			if err == nil && len(raw) <= 4096 && jsonx.Decode(raw, &refusal) == nil && json.Unmarshal(raw, &fields) == nil && refusal.Message == "Request rate limit exceeded, please re-evaluate your request patterns" && string(fields["data"]) == "null" {
				reason = "http-rate-limited"
			}
		}
		return writer.Attempt{Rejected: &writer.Rejection{Reason: reason}}
	}
	if count == 0 {
		return writer.Attempt{}
	}
	raw, err := io.ReadAll(io.LimitReader(reply.Body, (1<<20)+1))
	if err != nil || len(raw) > 1<<20 {
		return lost
	}
	if count < 0 {
		var row struct {
			ID string `json:"id"`
		}
		if jsonx.Decode(raw, &row) != nil || len(row.ID) != 36 || uuid.Validate(row.ID) != nil {
			return lost
		}
		return writer.Attempt{IDs: []string{row.ID}}
	}
	var rows []struct {
		ID string `json:"id"`
	}
	if jsonx.Decode(raw, &rows) != nil || len(rows) != count {
		return lost
	}
	out, seen := writer.Attempt{}, map[string]bool{}
	for _, row := range rows {
		if len(row.ID) != 36 || uuid.Validate(row.ID) != nil || seen[row.ID] {
			return lost
		}
		seen[row.ID] = true
		out.IDs = append(out.IDs, row.ID)
	}
	return out
}

// batches never creates a singleton, including a final remainder of one.
func batches(n, limit uint64) ([]int, error) {
	if n < 2 || limit < 2 {
		return nil, instanceIssue("batch counts must be at least two")
	}
	out := []int{}
	for n > 0 {
		size := min(n, limit)
		if n-size == 1 {
			size--
		}
		if size < 2 {
			return nil, instanceIssue("cannot partition into legal batches")
		}
		out = append(out, int(size))
		n -= size
	}
	return out, nil
}
