// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package cmd implements the flow CLI commands using Cobra. It provides
// subcommands for rack, component, firmware, power, rule, and ingest
// operations, as well as a serve subcommand that starts the gRPC server.
package cmd

import (
	"os"

	"github.com/spf13/cobra"

	pkgcerts "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/certs"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/client"
)

// Flag names for the global persistent flags.
const (
	flagHost = "host"
	flagPort = "port"
)

// Global persistent flags inherited by all subcommands. Host and port
// configure the gRPC client target; cert flags configure mTLS for both client
// commands and the serve listener.
var (
	globalHost       string
	globalPort       int
	globalCACert     string
	globalTLSCert    string
	globalTLSKey     string
	globalServerName string
)

// rootCmd represents the base command when called without any subcommands
var rootCmd = &cobra.Command{
	Use:   "flow",
	Short: "NICo Flow CLI",
	Long:  `Command to manage and query racks, components, firmware, and operation rules in NICo Flow.`,
}

// Execute adds all child commands to the root command and sets flags appropriately.
// This is called by main.main(). It only needs to happen once to the rootCmd.
func Execute() {
	err := rootCmd.Execute()
	if err != nil {
		os.Exit(1)
	}
}

func init() {
	rootCmd.PersistentFlags().StringVarP(&globalHost, flagHost, "H", "localhost", "Flow server host")
	rootCmd.PersistentFlags().IntVarP(&globalPort, flagPort, "P", defaultServicePort, "Flow server port")
	rootCmd.PersistentFlags().StringVar(&globalCACert, "ca-cert", "", "Path to CA certificate file")
	rootCmd.PersistentFlags().StringVar(&globalTLSCert, "tls-cert", "", "Path to TLS certificate file")
	rootCmd.PersistentFlags().StringVar(&globalTLSKey, "tls-key", "", "Path to TLS private key file")
	rootCmd.PersistentFlags().StringVar(&globalServerName, "server-name", "", "Server name for TLS verification; when empty, TLS uses the dial target hostname")
	rootCmd.MarkFlagsRequiredTogether("ca-cert", "tls-cert", "tls-key")
}

// newGlobalClientConfig builds a client.Config from the global persistent flags.
func newGlobalClientConfig() client.Config {
	return client.Config{
		Host:       globalHost,
		Port:       globalPort,
		ServerName: globalServerName,
		CertConfig: pkgcerts.Config{
			CACert:  globalCACert,
			TLSCert: globalTLSCert,
			TLSKey:  globalTLSKey,
		},
	}
}
