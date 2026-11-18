/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
use std::net::SocketAddrV4;

use carbide_dhcp_server::errors::DhcpError;
use clap::{Parser, ValueEnum};

#[derive(Parser, Debug, Clone)]
#[clap(name = "forge-dhcp-server")]
#[clap(author = "https://github.com/NVIDIA/infra-controller")]
pub(super) struct Args {
    #[arg(long, help = "Interface name where to bind this server.")]
    pub(super) interfaces: Vec<String>,

    #[arg(
        long,
        help = "UDP address where the DHCP server listens.",
        default_value = "0.0.0.0:67"
    )]
    pub(super) listen_addr: SocketAddrV4,

    #[arg(
        long,
        help = "UDP destination port for responses to DHCP relays.",
        default_value_t = 67
    )]
    pub(super) relay_response_port: u16,

    #[arg(
        long,
        help = "DHCP config file path. The server saves its identity in <path>.duid at startup, \
                or on first application when gRPC starts without a live config. Its directory must support \
                durable writes and hard links. Preserve <path>.duid across upgrades.",
        default_value = "/var/support/forge-dhcp/conf/dhcp.yaml"
    )]
    pub(super) dhcp_config: String,

    #[arg(
        long,
        value_name = "CANDIDATE_YAML",
        conflicts_with_all = ["grpc_listen_addr", "metrics_listen_addr"],
        help = "Validate candidate DHCP YAML, then exit without writes or sockets. DPU mode requires --host-config. \
                --dhcp-config identifies the live config and <path>.duid used to preserve server identity. \
                Exit zero means valid configuration, not packet-serving readiness."
    )]
    pub(super) validate_config: Option<String>,

    #[arg(
        long,
        help = "DPU Agent provided input file path for IP selection. Defaults to \
                /var/support/forge-dhcp/conf/host.yaml when --grpc-listen-addr is set."
    )]
    pub(super) host_config: Option<String>,

    #[arg(long, help = "Root CA certificate used to connect to the Carbide API.")]
    pub(super) forge_root_ca_path: Option<String>,

    #[arg(
        long,
        requires = "client_key_path",
        help = "Client certificate used to connect to the Carbide API."
    )]
    pub(super) client_cert_path: Option<String>,

    #[arg(
        long,
        requires = "client_cert_path",
        help = "Client private key used to connect to the Carbide API."
    )]
    pub(super) client_key_path: Option<String>,

    #[arg(short, long, value_enum, default_value_t=ServerMode::Dpu)]
    pub(super) mode: ServerMode,

    #[arg(
        long,
        help = "gRPC server listen address for config hot-reload (e.g. 0.0.0.0:50051). \
                When omitted the gRPC server is not started and config reload is disabled."
    )]
    pub(super) grpc_listen_addr: Option<String>,

    #[arg(
        long,
        help = "HTTP listen address for the metrics/health endpoint (e.g. 0.0.0.0:9090). \
                When omitted the endpoint is not served; metrics are still collected."
    )]
    pub(super) metrics_listen_addr: Option<String>,
}

#[derive(ValueEnum, Clone, Debug)]
pub(super) enum ServerMode {
    Dpu,
    Controller,
}

impl Args {
    pub(super) fn load() -> Self {
        Self::parse()
    }

    pub(super) fn validate_interfaces(&self) -> Result<(), DhcpError> {
        if matches!(self.mode, ServerMode::Controller) && self.interfaces.len() > 1 {
            return Err(DhcpError::MultipleInterfacesProvidedOneSupported(
                self.interfaces.len(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use carbide_dhcp_server::errors::DhcpError;
    use clap::Parser;

    use super::Args;

    #[test]
    fn two_interfaces_are_supported_only_in_dpu_mode() {
        for (mode, expected) in [("dpu", Ok(())), ("controller", Err(2))] {
            let args = Args::try_parse_from([
                "forge-dhcp-server",
                "--mode",
                mode,
                "--interfaces",
                "eth0",
                "--interfaces",
                "eth1",
            ])
            .unwrap();
            let result = args.validate_interfaces().map_err(|error| match error {
                DhcpError::MultipleInterfacesProvidedOneSupported(count) => count,
                error => panic!("unexpected interface validation error: {error}"),
            });
            assert_eq!(result, expected, "{mode}");
        }
    }

    #[test]
    fn dhcp_port_arguments() {
        let defaults = Args::try_parse_from(["forge-dhcp-server"]).unwrap();
        assert_eq!(
            defaults.listen_addr,
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 67)
        );
        assert_eq!(defaults.relay_response_port, 67);
        assert_eq!(defaults.forge_root_ca_path, None);
        assert_eq!(defaults.client_cert_path, None);
        assert_eq!(defaults.client_key_path, None);

        let overridden = Args::try_parse_from([
            "forge-dhcp-server",
            "--listen-addr",
            "127.0.0.1:6767",
            "--relay-response-port",
            "6768",
        ])
        .unwrap();
        assert_eq!(
            overridden.listen_addr,
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, 6767)
        );
        assert_eq!(overridden.relay_response_port, 6768);

        assert!(
            Args::try_parse_from(["forge-dhcp-server", "--listen-addr", "[::]:6767",]).is_err()
        );

        let tls = Args::try_parse_from([
            "forge-dhcp-server",
            "--forge-root-ca-path",
            "/local/ca.crt",
            "--client-cert-path",
            "/local/client.crt",
            "--client-key-path",
            "/local/client.key",
        ])
        .unwrap();
        assert_eq!(tls.forge_root_ca_path.as_deref(), Some("/local/ca.crt"));
        assert_eq!(tls.client_cert_path.as_deref(), Some("/local/client.crt"));
        assert_eq!(tls.client_key_path.as_deref(), Some("/local/client.key"));

        assert!(
            Args::try_parse_from([
                "forge-dhcp-server",
                "--client-cert-path",
                "/local/client.crt",
            ])
            .is_err()
        );
    }

    #[test]
    fn validation_cannot_start_grpc_or_metrics_listeners() {
        for listener in ["--grpc-listen-addr", "--metrics-listen-addr"] {
            let error = Args::try_parse_from([
                "forge-dhcp-server",
                "--validate-config",
                "candidate.yaml",
                listener,
                "127.0.0.1:0",
            ])
            .expect_err("validation must not start listeners");
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }
}
