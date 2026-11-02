// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! VPC checks for values and routing policy returned by a pool provider.

use model::resource_pool::remote::ProviderRouteTargetPolicy;

use crate::CarbideError;

/// `VNI_SLOT` identifies the VPC's VNI in its allocation request. Each remote
/// request saves it, and ownership lookup matches it, so renaming it would hide
/// existing VPC allocations from deletion and leave their provider values held.
pub(super) const VNI_SLOT: &str = "vni";

/// `validate` accepts only a usable 24-bit VNI. Provider routing policy is rejected
/// because VPC creation does not save it or include it in DPU configuration.
pub(super) fn validate(
    vni: i32,
    route_target_policy: Option<&ProviderRouteTargetPolicy>,
) -> Result<(), CarbideError> {
    if route_target_policy.is_some() {
        return Err(CarbideError::FailedPrecondition(
            "provider route-target policy is not supported for VPC creation".to_owned(),
        ));
    }
    if !(1..=0x00ff_ffff).contains(&vni) {
        return Err(CarbideError::internal(format!(
            "VPC VNI provider returned `{vni}`; expected a value between 1 and 16777215",
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::{Case, check_cases};
    use model::resource_pool::remote::ProviderRouteTargetMode;

    use super::*;

    /// Provider assignments must fit VPC consumers before VPC creation commits
    /// them. Integrated pools retain their existing value rules.
    #[test]
    fn provider_assignments_must_be_usable_vnis() {
        check_cases(
            [
                Case {
                    scenario: "first usable VNI",
                    input: (1, None),
                    expect: Yields(()),
                },
                Case {
                    scenario: "last usable VNI",
                    input: (0x00ff_ffff, None),
                    expect: Yields(()),
                },
                Case {
                    scenario: "zero is not a VNI",
                    input: (0, None),
                    expect: FailsWith(tonic::Code::Internal),
                },
                Case {
                    scenario: "negative integer is not a VNI",
                    input: (-1, None),
                    expect: FailsWith(tonic::Code::Internal),
                },
                Case {
                    scenario: "integer exceeds the 24-bit VNI limit",
                    input: (0x0100_0000, None),
                    expect: FailsWith(tonic::Code::Internal),
                },
                Case {
                    scenario: "provider routing policy cannot be silently discarded",
                    input: (
                        1,
                        Some(ProviderRouteTargetPolicy {
                            mode: ProviderRouteTargetMode::Preserve,
                            imports: None,
                            exports: None,
                            peer_identity: Vec::new(),
                        }),
                    ),
                    expect: FailsWith(tonic::Code::FailedPrecondition),
                },
            ],
            |(value, route_target_policy)| {
                validate(value, route_target_policy.as_ref())
                    .map_err(|error| tonic::Status::from(error).code())
            },
        );
    }
}
