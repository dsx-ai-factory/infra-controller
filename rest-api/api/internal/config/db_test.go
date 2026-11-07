// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"reflect"
	"testing"

	cconfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"
)

func TestNewDBConfig(t *testing.T) {
	type args struct {
		host     string
		port     int
		name     string
		user     string
		password string
	}

	dbcfg := cconfig.DBConfig{
		Host:     "localhost",
		Port:     5432,
		Name:     "nico",
		User:     "nico",
		Password: "test123",
	}

	tests := []struct {
		name string
		args args
		want *cconfig.DBConfig
	}{
		{
			name: "initialize database config",
			args: args{
				host:     dbcfg.Host,
				port:     dbcfg.Port,
				name:     dbcfg.Name,
				user:     dbcfg.User,
				password: dbcfg.Password,
			},
			want: &dbcfg,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := cconfig.NewDBConfig(tt.args.host, tt.args.port, tt.args.name, tt.args.user, tt.args.password)

			if !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewDBConfig() = %v, want %v", got, tt.want)
			}

			if got := got.GetHostPort(); got != tt.want.GetHostPort() {
				t.Errorf("GetHostPort() = %v, want %v", got, tt.want.GetHostPort())
			}
		})
	}
}
