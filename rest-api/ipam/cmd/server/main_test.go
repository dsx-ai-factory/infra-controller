/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package main

import (
	"testing"

	"github.com/stretchr/testify/require"
	"go.mongodb.org/mongo-driver/mongo/options"
)

func TestMongoURI(t *testing.T) {
	cases := []struct {
		name string
		host string
		want string
	}{
		{name: "IPv6", host: "2001:db8::1", want: "[2001:db8::1]:27018"},
		{name: "scoped IPv6", host: "fe80::1%eth0", want: "[fe80::1%eth0]:27018"},
		{name: "bracketed IPv6", host: "[2001:db8::1]", want: "[2001:db8::1]:27018"},
		{name: "IPv4", host: "192.0.2.1", want: "192.0.2.1:27018"},
		{name: "hostname", host: "mongo.example.test", want: "mongo.example.test:27018"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			opts := options.Client().ApplyURI(mongoURI(tc.host, "27018"))
			require.NoError(t, opts.Validate())
			require.Equal(t, []string{tc.want}, opts.Hosts)
		})
	}
}
