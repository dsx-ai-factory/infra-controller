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

//! Merges an operator-supplied supplemental NVUE config into the agent's
//! generated config before it is applied, so the supplemental content is part
//! of the same atomic NVUE revision rather than a second, racing write.
//!
//! The supplemental document is an RFC 7386 JSON Merge Patch over the `set`
//! section of the generated startup config: objects merge recursively, any
//! other value replaces the target, and `null` deletes a key. Merge-patch
//! suits NVUE because NVUE models collections as objects-keyed-by-name, not
//! arrays. The merged `set` section must still deserialize as
//! [`nvue_client::config::NvueConfig`], whose `deny_unknown_fields` rejects
//! patches that introduce NVUE sections the agent does not model.

use eyre::WrapErr;
use nvue_client::config::NvueConfigWithHeader;

/// Applies the JSON merge-patch in `patch_json` to the `set` section of the
/// YAML startup config in `nvue_yaml` and returns the merged YAML document.
/// The `header` entry is passed through untouched.
///
/// The merge itself is `json_patch::merge` (RFC 7386). Because it operates on
/// `serde_json::Value`, the `set` section crosses into the JSON domain and
/// back; the return crossing MUST go through JSON text rather than
/// `serde_yaml::to_value`, because with the workspace-unified
/// `arbitrary_precision` feature a `serde_json` number serializes into YAML
/// as a `{"$serde_json::private::Number": ...}` mapping, corrupting the
/// config. `preserves_scalar_types` is the regression test for this bridge.
pub(crate) fn merge_into_nvue_yaml(nvue_yaml: &str, patch_json: &str) -> eyre::Result<String> {
    let patch: serde_json::Value = serde_json::from_str(patch_json)
        .wrap_err("supplemental network config is not valid JSON")?;
    if !patch.is_object() {
        return Err(eyre::eyre!(
            "supplemental network config must be a JSON object of NVUE config sections"
        ));
    }

    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(nvue_yaml).wrap_err("parsing generated NVUE config as YAML")?;
    let entries = doc
        .as_sequence_mut()
        .ok_or_else(|| eyre::eyre!("generated NVUE config is not a YAML sequence"))?;
    let set_key = serde_yaml::Value::from("set");
    let set_entry = entries
        .iter_mut()
        .find_map(|entry| entry.as_mapping_mut().and_then(|map| map.get_mut(&set_key)))
        .ok_or_else(|| eyre::eyre!("generated NVUE config has no 'set' entry"))?;

    let mut merged =
        serde_json::to_value(&*set_entry).wrap_err("converting the NVUE 'set' section to JSON")?;
    json_patch::merge(&mut merged, &patch);

    // Bridge back through JSON text, not serde_yaml::to_value — see above.
    let merged_json =
        serde_json::to_string(&merged).wrap_err("serializing the merged NVUE 'set' section")?;
    *set_entry = serde_yaml::from_str(&merged_json)
        .wrap_err("converting the merged NVUE 'set' section back to YAML")?;

    let merged = serde_yaml::to_string(&doc).wrap_err("serializing the merged NVUE config")?;

    // Reject unknown top-level NVUE sections (deny_unknown_fields) instead of
    // pushing a config NVUE may fail on as a whole-DPU apply. This is the same
    // parse the REST apply path performs on the final document.
    NvueConfigWithHeader::from_yaml(&merged)
        .wrap_err("merged NVUE config failed shape validation")?;

    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_NVUE_YAML: &str = r#"- header:
    model: BLUEFIELD
    nvue-api-version: nvue_v1
    rev-id: 1.0
    version: HBN 2.4.0
- set:
    interface:
      lo:
        ip:
          address:
            10.0.0.1/32: {}
        type: loopback
    vrf:
      default:
        router:
          bgp:
            autonomous-system: 65001
"#;

    #[test]
    fn merge_adds_new_vrf_and_interface() {
        let patch = r#"{
            "vrf": {"storage": {"router": {"bgp": {"autonomous-system": 65099}}}},
            "interface": {"pf0sf4": {"type": "swp"}}
        }"#;
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        let set = &doc[1]["set"];
        // Existing content survives alongside the additions.
        assert_eq!(set["interface"]["lo"]["type"], "loopback");
        assert_eq!(set["interface"]["pf0sf4"]["type"], "swp");
        assert_eq!(
            set["vrf"]["default"]["router"]["bgp"]["autonomous-system"],
            65001
        );
        assert_eq!(
            set["vrf"]["storage"]["router"]["bgp"]["autonomous-system"],
            65099
        );
    }

    #[test]
    fn merge_overrides_nested_value() {
        let patch = r#"{"vrf": {"default": {"router": {"bgp": {"autonomous-system": 65002}}}}}"#;
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        assert_eq!(
            doc[1]["set"]["vrf"]["default"]["router"]["bgp"]["autonomous-system"],
            65002
        );
    }

    #[test]
    fn merge_null_deletes_key() {
        let patch = r#"{"vrf": {"default": null}}"#;
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        // The key must be removed, not left behind with a null value —
        // indexing alone cannot tell those apart.
        let vrf = doc[1]["set"]["vrf"]
            .as_mapping()
            .expect("vrf mapping survives");
        assert!(!vrf.contains_key(serde_yaml::Value::from("default")));
        // The sibling section is untouched.
        assert_eq!(doc[1]["set"]["interface"]["lo"]["type"], "loopback");
    }

    #[test]
    fn merge_preserves_header() {
        let patch = r#"{"vrf": {"storage": {}}}"#;
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        assert_eq!(doc[0]["header"]["model"], "BLUEFIELD");
        assert_eq!(doc[0]["header"]["nvue-api-version"], "nvue_v1");
    }

    #[test]
    fn merged_result_still_parses_as_wire_config() {
        let patch = r#"{"vrf": {"storage": {}}}"#;
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        nvue_client::config::NvueConfigWithHeader::from_yaml(&merged)
            .expect("merged document must remain a valid NVUE startup config");
    }

    #[test]
    fn rejects_unknown_top_level_section() {
        let patch = r#"{"qos": {"egress": {}}}"#;
        let err = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap_err();
        assert!(err.to_string().contains("shape validation"), "{err:#}");
    }

    #[test]
    fn rejects_non_object_patch() {
        let err = merge_into_nvue_yaml(BASE_NVUE_YAML, "[1, 2]").unwrap_err();
        assert!(err.to_string().contains("JSON object"), "{err:#}");
    }

    #[test]
    fn rejects_invalid_json() {
        let err = merge_into_nvue_yaml(BASE_NVUE_YAML, "{not json").unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "{err:#}");
    }

    /// The contract is JSON Merge Patch; YAML-only syntax must not slip
    /// through just because the merge internally parses with serde_yaml.
    #[test]
    fn rejects_yaml_only_patch_syntax() {
        for patch in [
            "vrf:\n  default: {}",               // block mapping
            "{vrf: {default: {}}}",              // unquoted flow-mapping keys
            "---\n{\"vrf\": {\"default\": {}}}", // document marker
        ] {
            let err = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap_err();
            assert!(
                err.to_string().contains("not valid JSON"),
                "patch {patch:?} should be rejected as non-JSON: {err:#}"
            );
        }
    }

    /// The supplemental document is RFC 7386 JSON *Merge* Patch (an object
    /// mirroring the config's shape), not RFC 6902 JSON Patch (an array of
    /// {op, path} operations). A 6902 document must be rejected, not applied.
    #[test]
    fn rejects_rfc6902_json_patch_document() {
        let patch = r#"[{"op": "add", "path": "/vrf/storage", "value": {}}]"#;
        let err = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap_err();
        assert!(err.to_string().contains("JSON object"), "{err:#}");
    }

    #[test]
    fn empty_patch_object_is_a_no_op() {
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, "{}").unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        let base: serde_yaml::Value = serde_yaml::from_str(BASE_NVUE_YAML).unwrap();
        assert_eq!(doc[1]["set"], base[1]["set"]);
    }

    /// A deep addition must not clobber siblings anywhere along the path.
    #[test]
    fn deep_merge_preserves_siblings_at_every_level() {
        let patch = r#"{"vrf": {"default": {"router": {"bgp": {"router-id": "10.0.0.1"}}}}}"#;
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        let bgp = &doc[1]["set"]["vrf"]["default"]["router"]["bgp"];
        assert_eq!(bgp["autonomous-system"], 65001);
        assert_eq!(bgp["router-id"], "10.0.0.1");
    }

    /// `null` deletes exactly the addressed key; the enclosing mapping and
    /// its siblings survive.
    #[test]
    fn null_deletes_nested_key_only() {
        let patch = r#"{"vrf": {"default": {"router": {"bgp": {"autonomous-system": null}}}}}"#;
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        let bgp = doc[1]["set"]["vrf"]["default"]["router"]["bgp"]
            .as_mapping()
            .expect("bgp mapping must survive the delete");
        assert!(!bgp.contains_key(serde_yaml::Value::from("autonomous-system")));
        assert_eq!(doc[1]["set"]["interface"]["lo"]["type"], "loopback");
    }

    /// RFC 7386: deleting a key that does not exist is not an error.
    #[test]
    fn null_on_missing_key_is_ignored() {
        let patch = r#"{"vrf": {"no-such-vrf": null}}"#;
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        assert_eq!(
            doc[1]["set"]["vrf"]["default"]["router"]["bgp"]["autonomous-system"],
            65001
        );
    }

    /// RFC 7386: a non-object patch value replaces the target wholesale, in
    /// both directions (scalar over object, object over scalar).
    #[test]
    fn non_object_values_replace_wholesale() {
        let patch = r#"{"interface": {"lo": {"type": {"mode": "l3"}, "ip": "disabled"}}}"#;
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        let lo = &doc[1]["set"]["interface"]["lo"];
        // object replaced the "loopback" scalar
        assert_eq!(lo["type"]["mode"], "l3");
        // scalar replaced the ip mapping wholesale (so the old address key
        // is gone with it)
        assert_eq!(lo["ip"], "disabled");
    }

    /// RFC 7386 does not merge arrays: a patch array replaces the target.
    #[test]
    fn arrays_are_replaced_not_merged() {
        let patch = r#"{"system": {"dns": {"server": ["1.1.1.1", "8.8.8.8"]}}}"#;
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        let servers = doc[1]["set"]["system"]["dns"]["server"]
            .as_sequence()
            .expect("array survives as a sequence");
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0], "1.1.1.1");
    }

    /// Scalar types must survive the merge unchanged. Guards against value
    /// re-encoding regressions (e.g. serde_json's arbitrary_precision feature
    /// turning numbers into private tagged mappings).
    #[test]
    fn preserves_scalar_types() {
        let patch = r#"{
            "vrf": {"t": {"num": 65099, "float": 1.5, "flag": true, "text": "swp", "big": 4294967296}}
        }"#;
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        assert!(
            !merged.contains("$serde_json"),
            "numbers must not re-encode as tagged mappings: {merged}"
        );
        let doc: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        let t = &doc[1]["set"]["vrf"]["t"];
        assert_eq!(t["num"], 65099);
        assert_eq!(t["float"], 1.5);
        assert_eq!(t["flag"], true);
        assert_eq!(t["text"], "swp");
        assert_eq!(t["big"], 4294967296u64);
    }

    /// NVUE keys are frequently prefix-like ("10.1.2.3/31"); they must pass
    /// through as plain string keys.
    #[test]
    fn accepts_prefix_style_keys() {
        let patch = r#"{"interface": {"pf0sf4_r": {"ip": {"address": {"10.1.2.3/31": {}}}}}}"#;
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        assert!(
            doc[1]["set"]["interface"]["pf0sf4_r"]["ip"]["address"]["10.1.2.3/31"].is_mapping()
        );
    }

    /// The storage test-bed's actual shape: add the storage VPC VRF, put the
    /// SF representor into it, and assign the link address — in one patch,
    /// alongside the generated config, still parseable by the wire model.
    #[test]
    fn realistic_storage_testbed_patch() {
        let patch = r#"{
            "vrf": {
                "storage-vpc": {
                    "router": {"bgp": {"autonomous-system": 65099, "router-id": "10.9.0.1"}}
                }
            },
            "interface": {
                "pf0sf4_r": {
                    "ip": {"vrf": "storage-vpc", "address": {"10.9.1.0/31": {}}},
                    "type": "swp"
                }
            }
        }"#;
        let merged = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        // Generated config intact, storage additions present, wire-parseable.
        let doc: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();
        let set = &doc[1]["set"];
        assert_eq!(set["interface"]["lo"]["type"], "loopback");
        assert_eq!(
            set["vrf"]["default"]["router"]["bgp"]["autonomous-system"],
            65001
        );
        assert_eq!(set["interface"]["pf0sf4_r"]["ip"]["vrf"], "storage-vpc");
        assert_eq!(
            set["vrf"]["storage-vpc"]["router"]["bgp"]["autonomous-system"],
            65099
        );
        nvue_client::config::NvueConfigWithHeader::from_yaml(&merged)
            .expect("merged document must remain a valid NVUE startup config");
    }

    /// Idempotence: applying the same patch to an already-patched document
    /// changes nothing — the loop the test-bed relies on when the agent
    /// re-renders with an unchanged file.
    #[test]
    fn merge_is_idempotent() {
        let patch = r#"{"vrf": {"storage": {"router": {"bgp": {"autonomous-system": 65099}}}}}"#;
        let once = merge_into_nvue_yaml(BASE_NVUE_YAML, patch).unwrap();
        let twice = merge_into_nvue_yaml(&once, patch).unwrap();
        assert_eq!(once, twice);
    }
}
