// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sites

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	fakecrdclient "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/client/clientset/versioned/fake"
)

// TestManager starts Site Manager over TLS with a serving certificate from a
// Cert Manager stub. Then it creates and reads a Site through the routes.
func TestManager(t *testing.T) {
	s, err := TestManagerCreateSite()
	require.NoError(t, err)
	t.Cleanup(s.Teardown)
	assert.NotEmpty(t, s.UUID1OTP)
}

// TestNewSiteManagerStopsRetryingWhenCanceled checks that a shutdown signal
// ends the wait for a serving certificate from an unreachable Cert Manager.
func TestNewSiteManagerStopsRetryingWhenCanceled(t *testing.T) {
	caPath := filepath.Join(t.TempDir(), "ca.pem")
	require.NoError(t, os.WriteFile(caPath, []byte(testCACert), 0600))

	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	o := Options{
		credsMgrURL:    "https://127.0.0.1:1",
		credsMgrCAPath: caPath,
		listenPort:     "0",
		namespace:      "site-manager",
		otpDuration:    time.Hour,
	}
	done := make(chan error, 1)
	go func() {
		_, err := NewSiteManager(ctx, o, fakecrdclient.NewSimpleClientset())
		done <- err
	}()

	select {
	case err := <-done:
		assert.ErrorIs(t, err, context.Canceled)
	case <-time.After(5 * time.Second):
		t.Fatal("NewSiteManager kept retrying after its context was canceled")
	}
}
