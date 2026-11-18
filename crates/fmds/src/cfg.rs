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

use std::net::{IpAddr, SocketAddr};

use clap::Parser;
use forge_dpu_agent_utils::FMDS_IPV6_ADDRESS;
use ipnetwork::{IpNetwork, Ipv6Network};

#[derive(Parser)]
#[clap(name = "carbide-fmds")]
pub struct Options {
    #[clap(long, default_value = "false", help = "Print version number and exit")]
    pub version: bool,

    /// gRPC listen address for receiving config updates from
    /// carbide-dpu-agent. The tenant should not be able to
    /// communicate with this address.
    #[clap(long, default_value = "0.0.0.0:50052")]
    pub grpc_address: SocketAddr,

    /// REST listen address for tenant OS metadata queries. Use [::]:80 to
    /// accept both IPv4 and IPv6 on port 80.
    #[clap(long, default_value = "0.0.0.0:80")]
    pub rest_address: SocketAddr,

    /// Optional additional REST listen address for compatibility. When
    /// omitted, FMDS listens only on `rest_address`.
    #[clap(long)]
    pub compatibility_rest_address: Option<SocketAddr>,

    /// Prometheus scrape address for `/metrics` (HTTP request stats for the REST metadata API).
    #[clap(long, default_value = "0.0.0.0:8888")]
    pub metrics_address: SocketAddr,

    /// Carbide API server address for phone_home.
    #[clap(long, default_value = "https://carbide-api.forge")]
    pub forge_api: String,

    /// Path to root CA certificate.
    /// This will probably be shared with the carbide-dpu-agent.
    #[clap(long)]
    pub root_ca: Option<String>,

    /// Path to client certificate.
    /// This will probably be shared with the carbide-dpu-agent.
    #[clap(long)]
    pub client_cert: Option<String>,

    /// Path to client key.
    /// This will probably be shared with the carbide-dpu-agent.
    #[clap(long)]
    pub client_key: Option<String>,

    /// Path of the dpu-agent's local API socket. When set, fmds fetches
    /// short-lived node-auth bearer JWTs from the agent and presents them to
    /// carbide-api — instead of (or in addition to) the mTLS client cert —
    /// so this pod no longer needs the machine private key mounted.
    #[clap(long, env = "FMDS_NODE_TOKEN_SOCKET")]
    pub node_token_socket: Option<String>,

    /// Name of the interface to assign the metadata-service address to.
    #[clap(long, env = "FMDS_INTERFACE_NAME", default_value = "f_pf0hpf_if")]
    pub interface_name: String,

    /// CIDR to assign on `interface_name` (cloud metadata-service address).
    /// IPv4 configures a reply gateway at the first host in the subnet.
    /// Custom IPv6 assignments require separately configured reply routing.
    #[clap(
        long,
        env = "FMDS_INTERFACE_CIDR",
        default_value = "169.254.169.254/30"
    )]
    pub interface_cidr: IpNetwork,

    /// Optional IPv6 metadata address alongside --interface-cidr. Only
    /// fd00:76b9::254/127 is supported, with HBN at fd00:76b9::255/127.
    /// Requires an IPv6 REST listener (for example, --rest-address '[::]:80')
    /// and matching IPv6 metadata routing on HBN. Changes require a restart.
    #[clap(long, env = "FMDS_INTERFACE_CIDR_V6", value_parser = parse_metadata_cidr_v6)]
    pub interface_cidr_v6: Option<Ipv6Network>,
}

impl Options {
    pub fn load() -> Self {
        Self::parse()
    }

    /// Reject an enabled IPv6 endpoint that neither REST listener would serve,
    /// before changing interface addresses or routes.
    pub fn validate(&self) -> eyre::Result<()> {
        // Only the additional metadata endpoint requires this fixed listener.
        // The family-neutral option also supports custom IPv6 assignments.
        if self.interface_cidr_v6.is_some()
            && !std::iter::once(self.rest_address)
                .chain(self.compatibility_rest_address)
                .any(|address| {
                    matches!(address.ip(), IpAddr::V6(ip)
                        if ip.is_unspecified() || ip == FMDS_IPV6_ADDRESS)
                })
        {
            eyre::bail!(
                "IPv6 metadata requires --rest-address or --compatibility-rest-address to listen on [::] or [{FMDS_IPV6_ADDRESS}]"
            );
        }
        Ok(())
    }
}

/// Keep the FMDS/HBN pair fixed so the two independently configured services
/// cannot silently choose different gateways from an arbitrary IPv6 prefix.
fn parse_metadata_cidr_v6(value: &str) -> Result<Ipv6Network, String> {
    let cidr = value
        .parse::<Ipv6Network>()
        .map_err(|error| error.to_string())?;
    if cidr.ip() != FMDS_IPV6_ADDRESS || cidr.prefix() != 127 {
        return Err(format!(
            "IPv6 metadata CIDR must be {FMDS_IPV6_ADDRESS}/127"
        ));
    }
    Ok(cidr)
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, FromArgMatches};

    use super::*;

    /// Keep CLI cases independent of operator environment overrides without
    /// changing process-global environment variables used by parallel tests.
    fn parse_options(arguments: &[&str]) -> Result<Options, clap::Error> {
        let matches = Options::command()
            .mut_arg("interface_cidr", |argument| argument.env(None::<&str>))
            .mut_arg("interface_cidr_v6", |argument| argument.env(None::<&str>))
            .try_get_matches_from(arguments)?;
        Options::from_arg_matches(&matches)
    }

    /// Omitted listener and IPv6 options preserve the default IPv4 endpoint
    /// without creating a second tenant-facing listener.
    #[test]
    fn compatibility_rest_address_is_disabled_by_default() {
        let options = parse_options(&["carbide-fmds"]).unwrap();

        assert_eq!(options.rest_address, "0.0.0.0:80".parse().unwrap());
        assert_eq!(options.compatibility_rest_address, None);
        assert_eq!(options.interface_cidr_v6, None);
        assert_eq!(
            options.interface_cidr,
            "169.254.169.254/30".parse().unwrap()
        );
    }

    /// Independent listener overrides keep operator-selected bind addresses
    /// and ports instead of deriving one endpoint from the other.
    #[test]
    fn rest_addresses_can_be_overridden_independently() {
        let options = parse_options(&[
            "carbide-fmds",
            "--rest-address",
            "127.0.0.1:8080",
            "--compatibility-rest-address",
            "127.0.0.1:7778",
        ])
        .unwrap();

        assert_eq!(options.rest_address, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(
            options.compatibility_rest_address,
            Some("127.0.0.1:7778".parse().unwrap())
        );
    }

    /// IPv6 is opt-in and must have a listener for the metadata address;
    /// accepting only loopback would leave tenant requests unanswered.
    #[test]
    fn ipv6_metadata_requires_a_matching_listener() {
        use carbide_test_support::Outcome::{FailsWith, Yields};
        use carbide_test_support::scenarios;

        let listener_error = format!(
            "IPv6 metadata requires --rest-address or --compatibility-rest-address to listen on [::] or [{FMDS_IPV6_ADDRESS}]"
        );
        scenarios!(run = |arguments: &[&str]| -> Result<(), String> {
            parse_options(arguments)
                .map_err(|error| error.to_string())?
                .validate()
                .map_err(|error| error.to_string())
        };
            // Omission preserves the IPv4-only deployment contract.
            "IPv4 defaults" {
                &["carbide-fmds"][..] => Yields(()),
            }
            // The wildcard accepts the endpoint without changing its port.
            "dual-stack REST" {
                &["carbide-fmds", "--interface-cidr-v6", "fd00:76b9::254/127", "--rest-address", "[::]:80"][..] => Yields(()),
            }
            // Custom IPv6 assignment does not opt into the fixed HBN peer.
            "custom IPv6 through the family-neutral CIDR option" {
                &["carbide-fmds", "--interface-cidr", "2001:db8::254/64", "--rest-address", "[2001:db8::254]:80"][..] => Yields(()),
                &["carbide-fmds", "--interface-cidr", "2001:db8::254/64"][..] => Yields(()),
            }
            // Repeating an address across options does not invalidate the opt-in.
            "same address in both CIDR options" {
                &["carbide-fmds", "--interface-cidr", "fd00:76b9::254/127", "--interface-cidr-v6", "fd00:76b9::254/127", "--rest-address", "[::]:80"][..] => Yields(()),
            }
            // A configured compatibility listener may serve the endpoint too.
            "explicit compatibility endpoint" {
                &["carbide-fmds", "--interface-cidr-v6", "fd00:76b9::254/127", "--compatibility-rest-address", "[fd00:76b9::254]:7777"][..] => Yields(()),
            }
            // Neither an IPv4 socket nor IPv6 loopback serves this address.
            "unreachable metadata listener" {
                &["carbide-fmds", "--interface-cidr-v6", "fd00:76b9::254/127"][..] => FailsWith(listener_error.clone()),
                &["carbide-fmds", "--interface-cidr-v6", "fd00:76b9::254/127", "--rest-address", "[::1]:80"][..] => FailsWith(listener_error),
            }
        );
    }

    /// Invalid opt-in CIDRs fail in Clap with the endpoint requirement, not
    /// later during address assignment or because a REST listener is missing.
    #[test]
    fn ipv6_metadata_cidr_parser_reports_the_supported_endpoint() {
        for cidr in [
            // A wider prefix changes the directly connected metadata link.
            "fd00:76b9::254/64",
            // The peer address belongs to HBN, not the metadata server.
            "fd00:76b9::255/127",
        ] {
            let error = parse_options(&["carbide-fmds", "--interface-cidr-v6", cidr])
                .err()
                .expect("unsupported metadata CIDR");
            assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
            assert!(
                error.to_string().contains(&format!(
                    "IPv6 metadata CIDR must be {FMDS_IPV6_ADDRESS}/127"
                )),
                "{error}"
            );
        }
    }
}
