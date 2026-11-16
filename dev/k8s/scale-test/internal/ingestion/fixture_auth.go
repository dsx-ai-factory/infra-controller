// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"
)

type fixtureAuth struct {
	TokenURL          string `yaml:"tokenUrl"`
	ProviderDirectory string `yaml:"providerDirectory"`
	TenantDirectory   string `yaml:"tenantDirectory"`
	TenantClientID    string `yaml:"tenantClientId"`
	CredentialsDir    string `yaml:"credentialsDir"`
}
type clientIdentity struct {
	ClientID     string `json:"clientId"`
	ClientSecret string `json:"clientSecret"`
}
type oauthToken struct {
	token string
	until time.Time
}
type restAuth struct {
	mu     sync.Mutex
	tokens map[bool]oauthToken
}

func boundedSecret(path string) ([]byte, error) {
	b, err := fixtureBytes(path, 16384)
	if err != nil || len(strings.TrimSpace(string(b))) == 0 {
		return nil, instanceIssue("credential file unavailable, invalid or too large")
	}
	return b, nil
}
func fixtureBytes(path string, limit int64) ([]byte, error) {
	f, err := os.OpenFile(path, os.O_RDONLY|syscall.O_NONBLOCK, 0)
	if err != nil {
		return nil, err
	}
	defer func() { _ = f.Close() }()
	info, err := f.Stat()
	if err != nil || !info.Mode().IsRegular() {
		return nil, instanceIssue("fixture file must be regular")
	}
	b, err := io.ReadAll(io.LimitReader(f, limit+1))
	if len(b) > int(limit) {
		return nil, instanceIssue("fixture file too large")
	}
	return b, err
}
func identityFrom(dir string) (clientIdentity, error) {
	var out clientIdentity
	id, err := boundedSecret(filepath.Join(dir, "client-id"))
	if err != nil {
		return out, err
	}
	secret, err := boundedSecret(filepath.Join(dir, "client-secret"))
	if err != nil {
		return out, err
	}
	out.ClientID, out.ClientSecret = strings.TrimSpace(string(id)), strings.TrimSpace(string(secret))
	return out, nil
}
func writeIdentity(dir, name string, id clientIdentity) error {
	if err := os.MkdirAll(dir, 0700); err != nil {
		return err
	}
	b, _ := json.Marshal(id)
	if len(b) > 16384 {
		return instanceIssue("credential identity too large")
	}
	f, err := os.CreateTemp(dir, name+".*.new")
	if err != nil {
		return err
	}
	temp := f.Name()
	defer func() { _ = os.Remove(temp) }()
	_, err = f.Write(b)
	err = errors.Join(err, f.Sync())
	closeErr := f.Close()
	if err = errors.Join(err, closeErr); err != nil {
		return err
	}
	if err = os.Rename(temp, filepath.Join(dir, name+".json")); err != nil {
		return err
	}
	return syncFixtureDir(dir)
}
func (r *restWrites) identity(provider bool) (clientIdentity, error) {
	name := "tenant"
	if provider {
		name = "provider"
	}
	b, err := boundedSecret(filepath.Join(r.settings.Rest.Auth.CredentialsDir, name+".json"))
	var id clientIdentity
	if err != nil {
		return id, err
	}
	if json.Unmarshal(b, &id) != nil || id.ClientID == "" || id.ClientSecret == "" {
		return id, instanceIssue("credential identity invalid")
	}
	return id, nil
}
func (r *restWrites) authorize(ctx context.Context, provider bool) error {
	if r.settings.Rest.Auth.TokenURL == "" {
		return r.refreshToken()
	}
	r.auth.mu.Lock()
	defer r.auth.mu.Unlock()
	if token := r.auth.tokens[provider]; time.Now().Add(30 * time.Second).Before(token.until) {
		r.token, r.until = token.token, token.until
		return nil
	}
	id, err := r.identity(provider)
	if err != nil {
		return err
	}
	token, err := r.grant(ctx, r.settings.Rest.Auth.TokenURL, url.Values{"grant_type": {"client_credentials"}, "client_id": {id.ClientID}, "client_secret": {id.ClientSecret}})
	if err != nil {
		return err
	}
	if r.auth.tokens == nil {
		r.auth.tokens = map[bool]oauthToken{}
	}
	r.auth.tokens[provider] = token
	r.token, r.until = token.token, token.until
	return nil
}
func validToken(until time.Time) error {
	if !until.IsZero() && !time.Now().Before(until) {
		return instanceIssue("credential token expired before dispatch")
	}
	return nil
}
func (r *restWrites) grant(ctx context.Context, target string, form url.Values) (oauthToken, error) {
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	for attempt := 0; ; attempt++ {
		token, retry, err := r.grantAttempt(ctx, target, form)
		if ctx.Err() != nil {
			err = identityError(ctx, err, "token request stopped")
		}
		if err == nil || !retry || attempt == 2 || ctx.Err() != nil {
			return token, err
		}
		timer := time.NewTimer(time.Second)
		select {
		case <-ctx.Done():
			timer.Stop()
			return oauthToken{}, identityError(ctx, ctx.Err(), "token retry stopped")
		case <-timer.C:
		}
	}
}
func (r *restWrites) grantAttempt(ctx context.Context, target string, form url.Values) (oauthToken, bool, error) {
	start := time.Now()
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, target, strings.NewReader(form.Encode()))
	if err != nil {
		return oauthToken{}, false, instanceIssue("token request invalid")
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	reply, err := r.identityHTTP().Do(req)
	if err != nil {
		return oauthToken{}, true, identityError(ctx, err, "token request failed")
	}
	defer func() { _ = reply.Body.Close() }()
	if reply.StatusCode != http.StatusOK {
		return oauthToken{}, reply.StatusCode == 429 || reply.StatusCode >= 500 && reply.StatusCode < 600, instanceIssue("token endpoint refused authentication")
	}
	raw, err := io.ReadAll(io.LimitReader(reply.Body, (1<<20)+1))
	var body struct {
		Token   string `json:"access_token"`
		Expires uint64 `json:"expires_in"`
	}
	if err != nil {
		return oauthToken{}, true, identityError(ctx, err, "token body read failed")
	}
	if len(raw) > 1<<20 || json.Unmarshal(raw, &body) != nil || body.Expires <= 30 || body.Expires > 86400 || len(body.Token) == 0 || len(body.Token) > 65536 || strings.ContainsFunc(body.Token, func(r rune) bool { return r < 33 || r > 126 }) {
		return oauthToken{}, false, instanceIssue("token reply invalid")
	}
	return oauthToken{body.Token, start.Add(time.Duration(body.Expires) * time.Second)}, false, nil
}

func (r *restWrites) identityHTTP() *http.Client {
	if r.authClient != nil {
		return r.authClient
	}
	return r.client
}

// Preserve only typed lifecycle causes, never HTTP errors containing secrets.
func identityError(ctx context.Context, err error, message string) error {
	cause := ctx.Err()
	if cause == nil {
		var timeout net.Error
		switch {
		case errors.Is(err, context.Canceled):
			cause = context.Canceled
		case errors.Is(err, context.DeadlineExceeded) || errors.As(err, &timeout) && timeout.Timeout():
			cause = context.DeadlineExceeded
		}
	}
	if cause != nil {
		return instanceObservationError(fmt.Errorf("%s: %w", message, cause))
	}
	return instanceIssue(message)
}

func (f *fixtureRun) tenantIdentity() error {
	s := f.in.t.s
	id, err := identityFrom(s.Rest.Auth.TenantDirectory)
	if err != nil {
		return err
	}
	if id.ClientID != s.Rest.Auth.TenantClientID {
		return instanceIssue("tenant credential identity changed")
	}
	return writeIdentity(s.Rest.Auth.CredentialsDir, "tenant", id)
}
