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

use std::collections::HashMap;

use carbide_api_core::test_support::default_config;
use carbide_test_harness::prelude::*;
use carbide_uuid::rack::RackProfileId;
use model::rack_type::{
    RackCapabilitiesSet, RackCapabilityCompute, RackCapabilityPowerShelf, RackCapabilitySwitch,
    RackHardwareClass, RackHardwareTopology, RackHardwareType, RackProductFamily, RackProfile,
    RackProfileConfig,
};

fn profile(
    product_family: RackProductFamily,
    rack_hardware_topology: RackHardwareTopology,
    compute_name: &str,
) -> RackProfile {
    RackProfile {
        product_family: Some(product_family),
        firmware_object: None,
        rack_hardware_type: Some(RackHardwareType::from("wiwynn_nvl72")),
        rack_hardware_topology: Some(rack_hardware_topology),
        rack_hardware_class: Some(RackHardwareClass::Prod),
        rack_capabilities: RackCapabilitiesSet {
            compute: RackCapabilityCompute {
                name: Some(compute_name.to_string()),
                count: 18,
                vendor: Some("Wiwynn".to_string()),
                slot_ids: Some(vec![11, 12]),
                attributes: Default::default(),
            },
            switch: RackCapabilitySwitch {
                name: Some("ND5200".to_string()),
                count: 9,
                vendor: Some("NVIDIA".to_string()),
                slot_ids: Some(vec![19, 20]),
                attributes: Default::default(),
            },
            power_shelf: RackCapabilityPowerShelf {
                name: Some("PowerShelf".to_string()),
                count: 8,
                vendor: Some("LiteOn".to_string()),
                slot_ids: Some(vec![6, 7]),
                attributes: Default::default(),
            },
        },
        attributes: Default::default(),
    }
}

async fn environment_with_profiles(
    pool: PgPool,
    rack_profiles: HashMap<String, RackProfile>,
) -> TestHarness {
    let mut config = default_config::get();
    config.rack_profiles = RackProfileConfig { rack_profiles };

    TestHarness::builder(pool)
        .with_api_builder_fn(move |builder| builder.with_runtime_config(config.into()))
        .build()
        .await
}

#[sqlx_test]
async fn list_rack_profiles_returns_configured_profiles_in_id_order(pool: PgPool) {
    let alpha_profile = profile(
        RackProductFamily::Gb200,
        RackHardwareTopology::Gb200Nvl72r1C2g4Topology,
        "GB200",
    );
    let zulu_profile = profile(
        RackProductFamily::Gb300,
        RackHardwareTopology::Gb300Nvl72r1C2g4Topology,
        "GB300",
    );
    let env = environment_with_profiles(
        pool,
        HashMap::from([
            ("zulu".to_string(), zulu_profile.clone()),
            ("alpha".to_string(), alpha_profile.clone()),
        ]),
    )
    .await;

    let response = env
        .api()
        .list_rack_profiles(tonic::Request::new(()))
        .await
        .expect("unable to list rack profiles")
        .into_inner();

    assert_eq!(response.rack_profiles.len(), 2);
    assert_eq!(
        response.rack_profiles[0].rack_profile_id,
        Some(RackProfileId::new("alpha"))
    );
    assert_eq!(
        response.rack_profiles[0].profile,
        Some((&alpha_profile).into())
    );
    assert_eq!(
        response.rack_profiles[1].rack_profile_id,
        Some(RackProfileId::new("zulu"))
    );
    assert_eq!(
        response.rack_profiles[1].profile,
        Some((&zulu_profile).into())
    );
}

#[sqlx_test]
async fn list_rack_profiles_returns_empty_list_for_empty_configuration(pool: PgPool) {
    let env = environment_with_profiles(pool, HashMap::new()).await;

    let response = env
        .api()
        .list_rack_profiles(tonic::Request::new(()))
        .await
        .expect("unable to list rack profiles")
        .into_inner();

    assert!(response.rack_profiles.is_empty());
}
