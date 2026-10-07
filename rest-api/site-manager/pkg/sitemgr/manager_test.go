// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sitemgr

import (
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestManager(t *testing.T) {
	s, err := TestManagerCreateSite()
	assert.Nil(t, err)
	assert.NotNil(t, s)
	err = s.TestManagerSiteTest()
	assert.NotNil(t, err)
	s.Teardown()
}

func TestCLI(t *testing.T) {
	cmd := NewCommand()
	assert.NotEqual(t, nil, cmd)
}

func TestNewCertManagerClient(t *testing.T) {
	srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
	}))
	defer srv.Close()

	servingCAPath, err := writeCAFile(srv.Certificate())
	require.NoError(t, err)
	defer os.Remove(servingCAPath)

	// A well-formed CA that simply did not sign the endpoint being dialled.
	// Every httptest server shares one built-in certificate, so a second
	// server would not be a distinct trust anchor.
	unrelatedCAPath := filepath.Join(t.TempDir(), "unrelated-ca.pem")
	require.NoError(t, os.WriteFile(unrelatedCAPath, []byte(testCACert), 0600))

	emptyPath := filepath.Join(t.TempDir(), "empty.pem")
	require.NoError(t, os.WriteFile(emptyPath, []byte("not a certificate"), 0600))

	tests := []struct {
		name string
		// caPath is the bundle handed to the client under test.
		caPath string
		// wantBuildErr means the client cannot be constructed at all, so the
		// process fails at startup rather than running unverified.
		wantBuildErr bool
		// wantRequestErr means the client is built but refuses the connection.
		wantRequestErr bool
	}{
		{
			name:   "CA that signed the endpoint is accepted",
			caPath: servingCAPath,
		},
		{
			name:           "unrelated CA is refused",
			caPath:         unrelatedCAPath,
			wantRequestErr: true,
		},
		{
			name:         "missing bundle fails at construction",
			caPath:       filepath.Join(t.TempDir(), "absent.pem"),
			wantBuildErr: true,
		},
		{
			name:         "bundle with no certificates fails at construction",
			caPath:       emptyPath,
			wantBuildErr: true,
		},
		{
			name:         "unset bundle fails at construction",
			caPath:       "",
			wantBuildErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			client, err := newCertManagerClient(tt.caPath)
			if tt.wantBuildErr {
				assert.Error(t, err)
				assert.Nil(t, client)
				return
			}
			require.NoError(t, err)

			resp, err := client.Get(srv.URL)
			if tt.wantRequestErr {
				assert.Error(t, err, "request should fail certificate verification")
				return
			}
			require.NoError(t, err)
			defer resp.Body.Close()
			assert.Equal(t, http.StatusOK, resp.StatusCode)
		})
	}
}
