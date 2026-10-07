use jsonpath_rust::JsonPath;

use super::Scenario;
use crate::artifact::ArtifactDownload;
use crate::client::RackFirmwareData;
use crate::ctx::RvsCtx;
use crate::error::RvsError;

/// Resolve all artifact download URLs for all scenarios.
///
/// For each scenario collects:
///   - OS image (`scenario.os.uri`)
///   - Direct-URI artifacts (`artifact.uri`)
///   - SOT-resolved artifacts (`artifact.sotpath`) evaluated via JSONPath
///
/// The caller is responsible for providing the SOT JSON when sotpath
/// artifacts are present.
pub fn resolve_artifact_urls<'a>(
    sot: &'a RackFirmwareData,
    ctx: &'a RvsCtx,
) -> Result<Vec<ArtifactDownload<'a>>, RvsError> {
    let mut downloads = vec![];

    for scenario in &ctx.scenarios {
        downloads.extend(resolve_for_scenario(
            sot,
            scenario,
            &ctx.cfg.artifact_cache.cache_dir,
        )?);
    }

    Ok(downloads)
}

fn resolve_for_scenario<'a>(
    sot: &'a RackFirmwareData,
    scenario: &'a Scenario,
    cache_dir: &str,
) -> Result<Vec<ArtifactDownload<'a>>, RvsError> {
    let ns = format!("{}/{}", scenario.rack.model, scenario.rack.sot_release);
    let mut downloads = vec![];

    // OS image
    downloads.push(ArtifactDownload {
        output_path: format!("{cache_dir}/{ns}/os"),
        url: scenario.os.uri.as_str(),
    });

    // Direct-URI artifacts
    for (artifact, url) in scenario
        .artifacts
        .iter()
        .filter_map(|a| Some((a, a.uri.as_deref()?)))
    {
        downloads.push(ArtifactDownload {
            output_path: format!("{cache_dir}/{ns}/{}", artifact.output),
            url,
        });
    }

    // SOT-resolved artifacts
    for (artifact, sotpath) in scenario
        .artifacts
        .iter()
        .filter_map(|a| Some((a, a.sotpath.as_deref()?)))
    {
        let url = eval_sotpath(sot, sotpath)?;
        downloads.push(ArtifactDownload {
            output_path: format!("{cache_dir}/{ns}/{}", artifact.output),
            url,
        });
    }

    Ok(downloads)
}

/// Evaluate a JSONPath expression against the SOT config and return the
/// first matching string value (the download URL), borrowed from the SOT.
fn eval_sotpath<'a>(sot: &'a RackFirmwareData, sotpath: &str) -> Result<&'a str, RvsError> {
    let results = sot
        .config
        .query_with_path(sotpath)
        .map_err(|e| RvsError::InvalidArg(format!("invalid sotpath '{sotpath}': {e}")))?;

    let value = results
        .into_iter()
        .next()
        .ok_or_else(|| RvsError::InvalidArg(format!("sotpath '{sotpath}' matched nothing")))?;

    value.val().as_str().ok_or_else(|| {
        RvsError::InvalidArg(format!("sotpath '{sotpath}' did not resolve to a string"))
    })
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use serde_json::json;

    use super::*;

    fn make_sot(config: serde_json::Value) -> RackFirmwareData {
        RackFirmwareData {
            id: "test".to_string(),
            config,
        }
    }

    fn make_scenario(model: &str, sot_release: &str) -> Scenario {
        Scenario {
            rack: super::super::RackTarget {
                model: model.to_string(),
                sot_release: sot_release.to_string(),
            },
            os: super::super::OsImage {
                uri: "https://example.com/os.img".to_string(),
            },
            artifacts: vec![],
            setup: vec![],
            test: vec![],
            teardown: vec![],
        }
    }

    #[test]
    fn eval_sotpath_resolves_to_the_first_matching_string() {
        /// A SOT config paired with the JSONPath to evaluate against it.
        struct Query {
            config: serde_json::Value,
            sotpath: &'static str,
        }

        scenarios!(
            run = |Query { config, sotpath }: Query| {
                // The match borrows `sot`, so resolve it to an owned String here.
                // RvsError isn't PartialEq; the failing rows assert only that the
                // path does not resolve, so carry the message as a String too.
                eval_sotpath(&make_sot(config), sotpath)
                    .map(str::to_string)
                    .map_err(|e| e.to_string())
            };
            "a string match resolves to its value" {
                Query {
                    config: json!({ "firmware": { "url": "https://cdn.example.com/fw.bin" } }),
                    sotpath: "$.firmware.url",
                } => Yields("https://cdn.example.com/fw.bin".to_string()),
            }
            "a path matching nothing is an error" {
                Query { config: json!({ "firmware": {} }), sotpath: "$.firmware.url" } => Fails,
            }
            "a non-string match is an error" {
                Query {
                    config: json!({ "firmware": { "version": 42 } }),
                    sotpath: "$.firmware.version",
                } => Fails,
            }
        );
    }

    #[test]
    fn resolve_for_scenario_collects_os_image_and_artifacts() {
        /// A SOT config and the single artifact (if any) to resolve alongside the
        /// always-present OS image.
        struct Case {
            config: serde_json::Value,
            artifact: Option<super::super::Artifact>,
        }

        // The OS image is always the first download; any artifact follows. Each
        // success row pins the full (output_path, url) list the resolution yields.
        scenarios!(
            run = |Case { config, artifact }: Case| {
                let sot = make_sot(config);
                let mut scenario = make_scenario("gb200nvl", "1.2.5");
                scenario.artifacts.extend(artifact);
                resolve_for_scenario(&sot, &scenario, "/cache")
                    .map(|downloads| {
                        downloads
                            .into_iter()
                            .map(|d| (d.output_path, d.url.to_string()))
                            .collect::<Vec<_>>()
                    })
                    // RvsError isn't PartialEq; the failing row asserts only that
                    // an unresolvable sotpath fails, so carry the message as a String.
                    .map_err(|e| e.to_string())
            };
            "the OS image alone is resolved when there are no artifacts" {
                Case { config: json!({}), artifact: None } => Yields(vec![(
                    "/cache/gb200nvl/1.2.5/os".to_string(),
                    "https://example.com/os.img".to_string(),
                )]),
            }
            "a direct-URI artifact follows the OS image" {
                Case {
                    config: json!({}),
                    artifact: Some(super::super::Artifact {
                        name: "diag".to_string(),
                        output: "diag.bin".parse().unwrap(),
                        uri: Some("https://example.com/diag.bin".to_string()),
                        sotpath: None,
                    }),
                } => Yields(vec![
                    (
                        "/cache/gb200nvl/1.2.5/os".to_string(),
                        "https://example.com/os.img".to_string(),
                    ),
                    (
                        "/cache/gb200nvl/1.2.5/diag.bin".to_string(),
                        "https://example.com/diag.bin".to_string(),
                    ),
                ]),
            }
            "a SOT-resolved artifact follows the OS image" {
                Case {
                    config: json!({ "packages": { "diag": "https://cdn.example.com/diag.bin" } }),
                    artifact: Some(super::super::Artifact {
                        name: "diag".to_string(),
                        output: "diag.bin".parse().unwrap(),
                        uri: None,
                        sotpath: Some("$.packages.diag".to_string()),
                    }),
                } => Yields(vec![
                    (
                        "/cache/gb200nvl/1.2.5/os".to_string(),
                        "https://example.com/os.img".to_string(),
                    ),
                    (
                        "/cache/gb200nvl/1.2.5/diag.bin".to_string(),
                        "https://cdn.example.com/diag.bin".to_string(),
                    ),
                ]),
            }
            "a SOT-resolved artifact whose path matches nothing is an error" {
                Case {
                    config: json!({}),
                    artifact: Some(super::super::Artifact {
                        name: "diag".to_string(),
                        output: "diag.bin".parse().unwrap(),
                        uri: None,
                        sotpath: Some("$.packages.diag".to_string()),
                    }),
                } => Fails,
            }
        );
    }
}
