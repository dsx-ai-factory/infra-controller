// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"crypto/tls"
	"fmt"
	"os"
	"testing"

	"github.com/stretchr/testify/assert"

	cconfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"
)

func TestNewTemporalConfig(t *testing.T) {
	type args struct {
		host          string
		port          int
		serverName    string
		namespace     string
		queue         string
		encryptionKey string
	}

	keyPath, certPath := SetupTestCerts(t)
	defer os.Remove(keyPath)
	defer os.Remove(certPath)

	tcfg := cconfig.TemporalConfig{
		Host:          "localhost",
		Port:          7233,
		ServerName:    "temporal.local",
		Namespace:     "cloud",
		Queue:         "cloud",
		EncryptionKey: "test",
		ClientTLSCfg: &tls.Config{
			ServerName:         fmt.Sprintf("%s.%s", "cloud", "temporal.local"),
			MinVersion:         tls.VersionTLS12,
			InsecureSkipVerify: false,
		},
	}

	tests := []struct {
		name string
		args args
		want *cconfig.TemporalConfig
	}{
		{
			name: "initialize Temporal config",
			args: args{
				host:          tcfg.Host,
				port:          tcfg.Port,
				serverName:    tcfg.ServerName,
				namespace:     tcfg.Namespace,
				queue:         tcfg.Queue,
				encryptionKey: tcfg.EncryptionKey,
			},
			want: &tcfg,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := cconfig.NewTemporalConfig(tt.args.host, tt.args.port, tt.args.serverName, tt.args.namespace, tt.args.queue, tt.args.encryptionKey, true, certPath, keyPath, certPath)
			assert.NoError(t, err)
			defer got.Close()

			if sn := got.ServerName; sn != tt.want.ServerName {
				t.Errorf("got.ServerName = %v, want %v", sn, tt.want.ServerName)
			}
			if got := got.GetHostPort(); got != tt.want.GetHostPort() {
				t.Errorf("GetHostPort() = %v, want %v", got, tt.want.GetHostPort())
			}
		})
	}
}
