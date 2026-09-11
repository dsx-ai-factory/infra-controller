// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certs

import (
	"net/http"
	"slices"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/core"
	authenticationv1 "k8s.io/api/authentication/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
)

// tokenAudience restricts service account tokens to the certificate issuance service.
const tokenAudience = "nico-rest-cert-manager"

func (s *Server) authorizeCertificateRequest(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		authorization := r.Header.Values("Authorization")
		var fields []string
		if len(authorization) == 1 {
			fields = strings.Fields(authorization[0])
		}
		if len(fields) != 2 || !strings.EqualFold(fields[0], "Bearer") {
			w.Header().Set("WWW-Authenticate", `Bearer realm="certificate-manager"`)
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		if s.TokenReviewer == nil || s.AllowedServiceAccount == "" {
			http.Error(w, "authentication unavailable", http.StatusServiceUnavailable)
			return
		}
		review, err := s.TokenReviewer.Create(r.Context(), &authenticationv1.TokenReview{
			Spec: authenticationv1.TokenReviewSpec{
				Token:     fields[1],
				Audiences: []string{tokenAudience},
			},
		}, metav1.CreateOptions{})
		if err != nil {
			core.GetLogger(r.Context()).WithError(err).Error("certificate issuer token validation failed")
			http.Error(w, "authentication unavailable", http.StatusServiceUnavailable)
			return
		}
		if !review.Status.Authenticated || !slices.Contains(review.Status.Audiences, tokenAudience) {
			w.Header().Set("WWW-Authenticate", `Bearer realm="certificate-manager"`)
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		if review.Status.User.Username != s.AllowedServiceAccount {
			http.Error(w, "forbidden", http.StatusForbidden)
			return
		}
		next.ServeHTTP(w, r)
	})
}
