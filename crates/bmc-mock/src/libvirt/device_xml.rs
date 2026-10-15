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

use std::path::PathBuf;

use eyre::WrapErr;
use serde::Serialize;
use url::Url;

use crate::{ActionError, VirtualMediaContents};

/// A virtual-media CD-ROM device with a validated media source.
/// Only this model's serializer constructs its libvirt device XML.
#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(rename = "disk")]
pub(super) struct DeviceXml {
    #[serde(rename = "@type")]
    kind: &'static str,
    #[serde(rename = "@device")]
    device: &'static str,
    driver: Driver,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<MediaSource>,
    target: Target,
    #[serde(skip_serializing_if = "Option::is_none")]
    readonly: Option<()>,
    alias: Alias,
}

impl DeviceXml {
    pub(super) fn virtual_media(
        device_id: &str,
        target: &str,
        contents: &VirtualMediaContents,
    ) -> eyre::Result<Self> {
        let (source, write_protected) = match contents {
            VirtualMediaContents::Empty => (None, false),
            VirtualMediaContents::Inserted {
                image,
                write_protected,
            } => (Some(MediaSource::parse(image)?), *write_protected),
        };
        Ok(Self {
            kind: match &source {
                Some(MediaSource::Network { .. }) => "network",
                _ => "file",
            },
            device: "cdrom",
            driver: Driver {
                name: "qemu",
                kind: "raw",
            },
            source,
            target: Target {
                dev: target.to_owned(),
                bus: "sata",
            },
            readonly: write_protected.then_some(()),
            alias: Alias {
                name: Self::media_alias(device_id),
            },
        })
    }

    pub(super) fn media_alias(device_id: &str) -> String {
        format!("ua-bmc-mock-vmedia-{device_id}")
    }

    pub(super) fn into_xml(self) -> eyre::Result<String> {
        quick_xml::se::to_string(&self).context("could not serialize libvirt device XML")
    }
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct Driver {
    #[serde(rename = "@name")]
    name: &'static str,
    #[serde(rename = "@type")]
    kind: &'static str,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct Target {
    #[serde(rename = "@dev")]
    dev: String,
    #[serde(rename = "@bus")]
    bus: &'static str,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct Alias {
    #[serde(rename = "@name")]
    name: String,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct Host {
    #[serde(rename = "@name")]
    name: String,
    #[serde(rename = "@port")]
    port: u16,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
enum MediaSource {
    File {
        #[serde(rename = "@file")]
        file: String,
    },
    Network {
        #[serde(rename = "@protocol")]
        protocol: String,
        #[serde(rename = "@name")]
        name: String,
        host: Host,
    },
}

impl MediaSource {
    fn file(path: PathBuf) -> Self {
        Self::File {
            file: path.to_string_lossy().into_owned(),
        }
    }

    fn parse(image: &str) -> Result<Self, ActionError> {
        let Ok(url) = Url::parse(image) else {
            return Ok(Self::file(PathBuf::from(image)));
        };
        match url.scheme() {
            "file" => url
                .to_file_path()
                .map(Self::file)
                .map_err(|()| ActionError::BadRequest(eyre::eyre!("invalid file URL: {}", image))),
            "http" | "https" => {
                if url.username() != "" || url.password().is_some() || url.query().is_some() {
                    return Err(ActionError::BadRequest(eyre::eyre!(
                        "virtual media URLs must not contain credentials or a query",
                    )));
                }
                let host = url.host().ok_or_else(|| {
                    ActionError::BadRequest(eyre::eyre!("virtual media URL has no host: {}", image))
                })?;
                // `Host::Display` brackets IPv6, but libvirt needs the bare address.
                let host = match host {
                    url::Host::Ipv6(address) => address.to_string(),
                    host => host.to_string(),
                };
                Ok(Self::Network {
                    protocol: url.scheme().to_string(),
                    name: url.path().to_string(),
                    host: Host {
                        name: host,
                        port: url
                            .port_or_known_default()
                            .expect("HTTP(S) has a default port"),
                    },
                })
            }
            scheme => Err(ActionError::BadRequest(eyre::eyre!(
                "unsupported virtual media URL scheme: {}",
                scheme
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_network_virtual_media_device() {
        let device = |source| {
            format!(
                "<disk type=\"network\" device=\"cdrom\"><driver name=\"qemu\" type=\"raw\"/>\
                {source}</source><target dev=\"sdb\" bus=\"sata\"/><readonly/>\
                <alias name=\"ua-bmc-mock-vmedia-Cd\"/></disk>"
            )
        };
        let cases = [
            (
                "IPv4 with an explicit port",
                "http://127.0.0.1:8080/installer.iso",
                device(
                    "<source protocol=\"http\" name=\"/installer.iso\"><host name=\"127.0.0.1\" port=\"8080\"/>",
                ),
            ),
            (
                "IPv6 with an explicit port",
                "http://[2001:db8::10]:8080/installer.iso",
                device(
                    "<source protocol=\"http\" name=\"/installer.iso\"><host name=\"2001:db8::10\" port=\"8080\"/>",
                ),
            ),
            (
                "domain with the default HTTPS port",
                "https://images.example.com/installer.iso",
                device(
                    "<source protocol=\"https\" name=\"/installer.iso\"><host name=\"images.example.com\" port=\"443\"/>",
                ),
            ),
        ];
        for (name, image, expected) in cases {
            let actual = DeviceXml::virtual_media(
                "Cd",
                "sdb",
                &VirtualMediaContents::Inserted {
                    image: image.to_owned(),
                    write_protected: true,
                },
            )
            .unwrap()
            .into_xml()
            .unwrap();
            assert_eq!(actual, expected, "{name}");
        }
    }

    #[test]
    fn builds_file_virtual_media_device() {
        let actual = DeviceXml::virtual_media(
            "ConfigCd",
            "sdc",
            &VirtualMediaContents::Inserted {
                image: "/tmp/config.iso".to_owned(),
                write_protected: false,
            },
        )
        .unwrap()
        .into_xml()
        .unwrap();

        assert!(actual.contains("<disk type=\"file\" device=\"cdrom\">"));
        assert!(actual.contains("<source file=\"/tmp/config.iso\"/>"));
        assert!(actual.contains("<target dev=\"sdc\" bus=\"sata\"/>"));
        assert!(!actual.contains("<readonly/>"));
    }
    #[test]
    fn empty_media_keeps_the_cdrom_without_a_source() {
        let xml = DeviceXml::virtual_media("Cd", "sdb", &VirtualMediaContents::Empty)
            .unwrap()
            .into_xml()
            .unwrap();
        assert_eq!(
            xml,
            "<disk type=\"file\" device=\"cdrom\"><driver name=\"qemu\" type=\"raw\"/><target dev=\"sdb\" bus=\"sata\"/><alias name=\"ua-bmc-mock-vmedia-Cd\"/></disk>"
        );
    }
}
