//
// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Standalone binary to issue gRPC calls to an NMX-C endpoint using libnmxc.
//

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
#[allow(unused_imports)]
use libnmxc::{Endpoint, NMX_C_GATEWAY_ID, Nmxc, NmxcClientPool, NmxcError, NmxcTlsConfig};
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "nmxc")]
#[command(about = "Interactive shell: issue gRPC calls to an NMX-C controller endpoint")]
struct Cli {
    /// gRPC endpoint URL (e.g. http://127.0.0.1:50051 or https://host:50051)
    #[arg(long, short, env = "NMXC_GRPC_ENDPOINT")]
    endpoint: String,

    /// Gateway ID sent on Hello and on all other RPCs (same as `hello` default)
    #[arg(long, env = "NMXC_GATEWAY_ID", default_value = NMX_C_GATEWAY_ID)]
    gateway_id: String,

    /// PEM file: extra CA bundle for verifying the NMX-C server (HTTPS; optional)
    #[arg(long, env = "NMXC_TLS_CA_CERT")]
    ca_cert: Option<PathBuf>,

    /// PEM file: client certificate for mTLS (optional; requires `--client-key`)
    #[arg(long, env = "NMXC_TLS_CLIENT_CERT")]
    client_cert: Option<PathBuf>,

    /// PEM file: client private key for mTLS (optional; requires `--client-cert`)
    #[arg(long, env = "NMXC_TLS_CLIENT_KEY")]
    client_key: Option<PathBuf>,

    /// TLS server name for SNI and verifying the server certificate (optional; defaults to the host in `--endpoint`)
    #[arg(long, env = "NMXC_TLS_AUTHORITY")]
    authority: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("libnmxc=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    match (&cli.client_cert, &cli.client_key) {
        (Some(_), Some(_)) | (None, None) => {}
        _ => {
            eprintln!("Error: --client-cert and --client-key must be given together for mTLS.");
            return ExitCode::FAILURE;
        }
    }

    if let Err(e) = run(cli).await {
        eprintln!("Error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn print_help() {
    println!(
        "TLS: use --ca-cert, --authority (optional; defaults to endpoint host), and/or --client-cert + --client-key (mTLS) with https://."
    );
    println!();
    println!("Commands:");
    println!(
        "  hello [gateway_id]     — Hello handshake; optional id updates the shell gateway for later commands"
    );
    println!(
        "  connect [url]          — Create a gRPC client (optional URL; default: shell endpoint)"
    );
    println!("  reconnect              — Drop the current client (disconnect)");
    println!("  domain-properties      — Get domain properties");
    println!("  domain-state           — Get domain state info");
    println!("  topology               — Get topology info");
    println!("  partition-count        — Get partition count");
    println!("  partition-info-list    — Get partition info list");
    println!("  gpu-info-list          — Get GPU info list");
    println!("  help, ?                — Show this help");
    println!("  quit, exit             — Leave the shell");
}

async fn run(cli: Cli) -> Result<(), NmxcError> {
    let mut builder = NmxcClientPool::builder();
    if cli.ca_cert.is_some()
        || cli.client_cert.is_some()
        || cli.client_key.is_some()
        || cli.authority.is_some()
    {
        builder = builder.tls(NmxcTlsConfig {
            ca_cert_path: cli.ca_cert.clone(),
            client_cert_path: cli.client_cert.clone(),
            client_key_path: cli.client_key.clone(),
            authority: cli.authority.clone(),
        });
    }
    let pool = builder.build()?;
    let mut endpoint_url = cli.endpoint;
    let mut client: Option<Box<dyn Nmxc>> =
        Some(pool.create_client(Endpoint::new(&endpoint_url)?).await?);

    let mut gateway_id = cli.gateway_id;

    let tls_note = if cli.client_cert.is_some() {
        " (mTLS)"
    } else if cli.ca_cert.is_some() {
        " (custom CA)"
    } else if cli.authority.is_some() {
        " (TLS authority)"
    } else {
        ""
    };
    println!(
        "NMX-C client shell. Connected endpoint: {} (gateway_id: {}){}",
        endpoint_url, gateway_id, tls_note
    );
    print_help();
    println!();

    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdin = stdin;
    let mut line = String::new();

    loop {
        print!("nmxc> ");
        io::stdout()
            .flush()
            .map_err(|e| NmxcError::invalid_response(e.to_string()))?;
        line.clear();
        let n = stdin
            .read_line(&mut line)
            .await
            .map_err(|e| NmxcError::invalid_response(e.to_string()))?;
        if n == 0 {
            println!();
            break;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        match parts[0].to_lowercase().as_str() {
            "quit" | "exit" => break,
            "help" | "?" => {
                print_help();
            }
            cmd => {
                if let Err(e) = execute_command(
                    &pool,
                    &mut endpoint_url,
                    &mut client,
                    cmd,
                    &parts[1..],
                    &mut gateway_id,
                )
                .await
                {
                    eprintln!("{e}");
                }
            }
        }
    }

    Ok(())
}

async fn execute_command(
    pool: &NmxcClientPool,
    endpoint_url: &mut String,
    client: &mut Option<Box<dyn Nmxc>>,
    cmd: &str,
    args: &[&str],
    gateway_id: &mut String,
) -> Result<(), NmxcError> {
    match cmd {
        "reconnect" => {
            *client = None;
            println!("Disconnected (endpoint was {}).", endpoint_url);
        }
        "connect" => {
            let url = args
                .first()
                .map(|s| s.to_string())
                .unwrap_or_else(|| endpoint_url.clone());
            *client = Some(pool.create_client(Endpoint::new(&url)?).await?);
            *endpoint_url = url;
            println!("Connected to {}.", endpoint_url);
        }
        cmd => {
            let Some(c) = client.as_mut() else {
                eprintln!("Not connected. Use 'connect' or 'connect <url>'.");
                return Ok(());
            };
            match cmd {
                "hello" => {
                    if let Some(g) = args.first() {
                        *gateway_id = g.to_string();
                    }
                    println!("issuing hello (gateway_id={})", gateway_id);
                    let response = c.hello(gateway_id.as_str()).await?;
                    let header = response
                        .server_header
                        .as_ref()
                        .ok_or_else(|| NmxcError::invalid_response("no server_header"))?;
                    println!("Hello OK: return_code={}", header.return_code);
                    if !response.components_ver.is_empty() {
                        println!("Components: {:?}", response.components_ver);
                    }
                    if !response.capabilities.is_empty() {
                        println!("Capabilities: {:?}", response.capabilities);
                    }
                }
                "domain-properties" => {
                    let response = c.get_domain_properties(None, gateway_id.as_str()).await?;
                    println!("Domain properties: {:?}", response);
                }
                "domain-state" => {
                    let response = c.get_domain_state_info(None, gateway_id.as_str()).await?;
                    println!("Domain state: {:?}", response);
                }
                "topology" => {
                    let response = c.get_topology_info(None, gateway_id.as_str()).await?;
                    println!("Topology: {} device(s)", response.device_topo_info.len());
                    for (i, dev) in response.device_topo_info.iter().enumerate() {
                        println!("  device[{}]: {:?}", i, dev.device);
                    }
                }
                "partition-count" => {
                    let req = libnmxc::nmxc_model::GetPartitionCountRequest {
                        context: Some(libnmxc::nmxc_model::Context {
                            context: String::new(),
                        }),
                        info_attr: 1, // NmxPartitionInfoAttrAll
                        num_gpus: 0,
                        num_nodes: 0,
                        health: 0, // NmxPartitionHealthUnknown
                        gateway_id: gateway_id.clone(),
                    };
                    let response = c.get_partition_count(req).await?;
                    println!("Partition count: {}", response.num_partitions);
                }
                "partition-info-list" => {
                    let req = libnmxc::nmxc_model::GetPartitionInfoListRequest {
                        context: Some(libnmxc::nmxc_model::Context {
                            context: String::new(),
                        }),
                        partition_id_list: vec![],
                        partition_name_list: vec![],
                        gateway_id: gateway_id.clone(),
                    };

                    let response = c.get_partition_info_list(req).await?;
                    println!("GetPartitionInfoList: {:?}", response.server_header);

                    println!("Partitions: {}", response.partition_info_list.len());
                    for p in &response.partition_info_list {
                        println!(
                            "  partition_id={:?} name={:?} num_gpus={}",
                            p.partition_id, p.name, p.num_gpus
                        );
                    }
                }
                "gpu-info-list" => {
                    let req = libnmxc::nmxc_model::GetGpuInfoListRequest {
                        context: Some(libnmxc::nmxc_model::Context {
                            context: String::new(),
                        }),
                        attr: 1, // NmxGpuAttrAll
                        num_gpus: 0,
                        loc: None,
                        partition_id: None,
                        gateway_id: gateway_id.clone(),
                        gpu_health: 0, // NmxGpuHealthUnknown
                    };
                    let response = c.get_gpu_info_list(req).await?;
                    println!("GPUs: {}", response.gpu_info_list.len());
                    for g in &response.gpu_info_list {
                        println!(
                            "  gpu_uid={} index={} health={:?}",
                            g.gpu_uid, g.gpu_id, g.gpu_health
                        );
                    }
                }
                _ => {
                    eprintln!(
                        "Unknown command {:?}. Type 'help' for a list of commands.",
                        cmd
                    );
                }
            }
        }
    }

    Ok(())
}
