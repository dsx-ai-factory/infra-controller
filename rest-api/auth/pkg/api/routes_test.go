// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"testing"

	"github.com/stretchr/testify/assert"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
)

func TestNewAuthRoutes(t *testing.T) {
	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	keycloakCfg := &config.KeycloakConfig{
		BaseURL:         "http://localhost:8082",
		ExternalBaseURL: "http://localhost:8082",
		ClientID:        "test-client",
		ClientSecret:    "test-secret",
		Realm:           "nico",
	}

	tests := []struct {
		name      string
		cfg       *config.KeycloakConfig
		session   *cdb.Session
		wantPaths []string
	}{
		{
			name:      "nil keycloak config returns nil",
			cfg:       nil,
			session:   dbSession,
			wantPaths: nil,
		},
		{
			name:      "nil database session returns nil",
			cfg:       keycloakCfg,
			session:   nil,
			wantPaths: nil,
		},
		{
			name:      "configured service and database session registers auth routes",
			cfg:       keycloakCfg,
			session:   dbSession,
			wantPaths: []string{"/login", "/callback", "/logout", "/refresh"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			routes := NewAuthRoutes(tt.cfg, tt.session)
			if tt.wantPaths == nil {
				assert.Nil(t, routes)
				return
			}
			gotPaths := make([]string, 0, len(routes))
			for _, r := range routes {
				assert.Equal(t, "POST", r.Method)
				assert.NotNil(t, r.Handler)
				gotPaths = append(gotPaths, r.Path)
			}
			assert.Equal(t, tt.wantPaths, gotPaths)
		})
	}
}
