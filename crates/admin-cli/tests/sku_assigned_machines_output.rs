/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#[path = "support/nic_firmware.rs"]
mod support;

use prost::Message as _;
use rpc::forge::{Sku, SkuIdList, SkuList, SkusByIdsRequest};
use support::{Exchange, assert_success, run_command};

#[tokio::test]
async fn sku_show_machines_preserves_assigned_order_and_empty_cells() {
    let machine_ids = [
        "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg",
        "fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530",
    ];
    let sku_ids = ["populated-sku", "empty-sku"];
    let output = run_command(
        &["sku", "show-machines"],
        None,
        vec![
            Exchange {
                method: rpc::service_path!("GetAllSkuIds"),
                request: Vec::new(),
                response: Ok(SkuIdList {
                    ids: sku_ids.map(str::to_owned).into(),
                }
                .encode_to_vec()),
            },
            Exchange {
                method: rpc::service_path!("FindSkusByIds"),
                request: SkusByIdsRequest {
                    ids: sku_ids.map(str::to_owned).into(),
                }
                .encode_to_vec(),
                response: Ok(SkuList {
                    skus: vec![
                        Sku {
                            id: sku_ids[0].to_string(),
                            associated_machine_ids: machine_ids
                                .iter()
                                .map(|id| id.parse().expect("valid machine ID"))
                                .collect(),
                            ..Default::default()
                        },
                        Sku {
                            id: sku_ids[1].to_string(),
                            ..Default::default()
                        },
                    ],
                }
                .encode_to_vec()),
            },
        ],
    )
    .await;
    assert_success(&output);
    let output = String::from_utf8(output.stdout).expect("UTF-8 CLI output");
    let rows: Vec<Vec<&str>> = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
        .map(|line| line.split('|').map(str::trim).collect())
        .collect();
    assert_eq!(
        rows,
        [
            vec!["SKU ID", "Assigned Machine IDs"],
            vec!["populated-sku", machine_ids[0]],
            vec!["", machine_ids[1]],
            vec!["empty-sku", ""],
        ],
        "{output}"
    );
}
