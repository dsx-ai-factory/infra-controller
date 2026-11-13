// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package harness

import (
	"fmt"
	"os"
	"reflect"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/forge"
)

// readSettings builds the nico-api client settings from the chart's SCALE_TEST_* variables. The trust domain, service and machine base paths and
// server name are required: an unset one returns an error naming the variable rather than falling back to a dev default, which would otherwise
// surface as a confusing TLS or trust-domain failure on a real site. CERT_DIR is optional and defaults to the mount the chart uses.
func readSettings() (forge.Settings, error) {
	v := map[string]string{}
	for _, key := range []string{"TRUST_DOMAIN", "SERVICE_BASE_PATHS", "MACHINE_BASE_PATH", "SERVER_NAME"} {
		val := os.Getenv("SCALE_TEST_" + key)
		if val == "" {
			return forge.Settings{}, fmt.Errorf("set SCALE_TEST_%s", key)
		}
		v[key] = val
	}
	certDir := os.Getenv("SCALE_TEST_CERT_DIR")
	if certDir == "" {
		certDir = forge.DefaultCertDir
	}
	return forge.Settings{
		CertDir:    certDir,
		Spiffe:     forge.SpiffeSettings{TrustDomain: v["TRUST_DOMAIN"], ServiceBasePaths: strings.Split(v["SERVICE_BASE_PATHS"], ","), MachineBasePath: v["MACHINE_BASE_PATH"]},
		ServerName: v["SERVER_NAME"],
	}, nil
}

func TestReadSettings(t *testing.T) {
	setAll := func(t *testing.T) {
		t.Setenv("SCALE_TEST_TRUST_DOMAIN", "nico.local")
		t.Setenv("SCALE_TEST_SERVICE_BASE_PATHS", "/ns/forge/sa/")
		t.Setenv("SCALE_TEST_MACHINE_BASE_PATH", "/machine/")
		t.Setenv("SCALE_TEST_SERVER_NAME", "forge.local")
		t.Setenv("SCALE_TEST_CERT_DIR", "")
	}
	t.Run("valid settings read", func(t *testing.T) {
		setAll(t)
		got, err := readSettings()
		want := forge.Settings{
			CertDir:    forge.DefaultCertDir,
			Spiffe:     forge.SpiffeSettings{TrustDomain: "nico.local", ServiceBasePaths: []string{"/ns/forge/sa/"}, MachineBasePath: "/machine/"},
			ServerName: "forge.local",
		}
		if err != nil || !reflect.DeepEqual(got, want) {
			t.Fatalf("got %+v, %v; want %+v, nil", got, err, want)
		}
	})
	// Each required variable, when unset, fails with its own name; a restored dev fallback would make the matching row pass with no error.
	for _, miss := range []string{"SCALE_TEST_TRUST_DOMAIN", "SCALE_TEST_SERVICE_BASE_PATHS", "SCALE_TEST_MACHINE_BASE_PATH", "SCALE_TEST_SERVER_NAME"} {
		t.Run("missing "+miss, func(t *testing.T) {
			setAll(t)
			t.Setenv(miss, "")
			_, err := readSettings()
			if err == nil || !strings.Contains(err.Error(), miss) {
				t.Errorf("want an error naming %s, got %v", miss, err)
			}
		})
	}
}
