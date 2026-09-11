// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certs

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	authenticationv1 "k8s.io/api/authentication/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	authenticationclient "k8s.io/client-go/kubernetes/typed/authentication/v1"
)

type recordingCertificateIssuer struct {
	CertificateIssuer
	requests []*CertificateRequest
}

func (i *recordingCertificateIssuer) NewCertificate(_ context.Context, req *CertificateRequest) (string, string, error) {
	i.requests = append(i.requests, req)
	return "test-certificate", "test-private-key", nil
}

type recordingTokenReviewer struct {
	authenticationclient.TokenReviewInterface
	status   authenticationv1.TokenReviewStatus
	err      error
	requests []*authenticationv1.TokenReview
}

func (r *recordingTokenReviewer) Create(_ context.Context, review *authenticationv1.TokenReview, _ metav1.CreateOptions) (*authenticationv1.TokenReview, error) {
	r.requests = append(r.requests, review)
	return &authenticationv1.TokenReview{Status: r.status}, r.err
}

func TestServer_PKICloudCertificateHandler(t *testing.T) {
	const siteManager = "system:serviceaccount:nico-rest:nico-rest-site-manager"
	for _, tc := range []struct {
		name          string
		authorization []string
		authenticated bool
		subject       string
		audience      string
		reviewErr     error
		wantReview    bool
		wantStatus    int
	}{
		{name: "missing credentials", wantStatus: http.StatusUnauthorized},
		{name: "invalid bearer token", authorization: []string{"Bearer invalid-token"}, wantReview: true, wantStatus: http.StatusUnauthorized},
		{name: "duplicate credentials", authorization: []string{"Bearer one", "Bearer two"}, wantStatus: http.StatusUnauthorized},
		{name: "default service account", authorization: []string{"Bearer token"}, authenticated: true, subject: "system:serviceaccount:nico-system:default", audience: tokenAudience, wantReview: true, wantStatus: http.StatusForbidden},
		{name: "site manager name in another namespace", authorization: []string{"Bearer token"}, authenticated: true, subject: "system:serviceaccount:nico-system:nico-rest-site-manager", audience: tokenAudience, wantReview: true, wantStatus: http.StatusForbidden},
		{name: "token intended for another audience", authorization: []string{"Bearer token"}, authenticated: true, subject: siteManager, audience: "kubernetes", wantReview: true, wantStatus: http.StatusUnauthorized},
		{name: "token review unavailable", authorization: []string{"Bearer token"}, reviewErr: errors.New("test API unavailable"), wantReview: true, wantStatus: http.StatusServiceUnavailable},
		{name: "authorized site manager", authorization: []string{"bearer token"}, authenticated: true, subject: siteManager, audience: tokenAudience, wantReview: true, wantStatus: http.StatusOK},
	} {
		t.Run(tc.name, func(t *testing.T) {
			issuer := &recordingCertificateIssuer{}
			reviewer := &recordingTokenReviewer{status: authenticationv1.TokenReviewStatus{
				Authenticated: tc.authenticated, User: authenticationv1.UserInfo{Username: tc.subject}, Audiences: []string{tc.audience},
			}, err: tc.reviewErr}
			server := &Server{Options: Options{TokenReviewer: reviewer, AllowedServiceAccount: siteManager}, certificateIssuer: issuer}
			handler := server.PKICloudCertificateHandler(context.Background())
			request := httptest.NewRequest(http.MethodPost, "https://cert-manager/v1/pki/cloud-cert",
				strings.NewReader(`{"name":"client","app":"00000000-0000-4000-8000-000000000001","ttl":24}`))
			request.Header["Authorization"] = tc.authorization
			response := httptest.NewRecorder()

			handler.ServeHTTP(response, request)

			assert.Equal(t, tc.wantStatus, response.Code)
			if tc.wantReview {
				require.Len(t, reviewer.requests, 1)
				assert.Equal(t, strings.Fields(tc.authorization[0])[1], reviewer.requests[0].Spec.Token)
				assert.Equal(t, []string{tokenAudience}, reviewer.requests[0].Spec.Audiences)
			} else {
				assert.Empty(t, reviewer.requests)
			}
			if tc.wantStatus == http.StatusOK {
				require.Len(t, issuer.requests, 1)
				assert.Equal(t, &CertificateRequest{Name: "client", App: "00000000-0000-4000-8000-000000000001", TTL: 24}, issuer.requests[0])
				assert.JSONEq(t, `{"key":"test-private-key","certificate":"test-certificate"}`, response.Body.String())
			} else {
				assert.Empty(t, issuer.requests, "rejected requests must not reach the signing operation")
				assert.NotContains(t, response.Body.String(), "test-private-key")
			}
		})
	}
}

func TestNewServerWithIssuer(t *testing.T) {
	for _, tc := range []struct {
		name      string
		options   Options
		wantError string
	}{
		{
			name:      "missing authorized identity",
			options:   Options{TokenReviewer: &recordingTokenReviewer{}},
			wantError: "allowed service account must have the form system:serviceaccount:namespace:name",
		},
		{
			name:      "missing token reviewer",
			options:   Options{AllowedServiceAccount: "system:serviceaccount:nico-rest:nico-rest-site-manager"},
			wantError: "token reviewer is required",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			server, err := NewServerWithIssuer(context.Background(), tc.options, &recordingCertificateIssuer{})
			require.EqualError(t, err, tc.wantError)
			assert.Nil(t, server)
		})
	}
}
