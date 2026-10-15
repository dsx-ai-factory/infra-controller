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

use std::str::FromStr;

use eyre::WrapErr;
use quick_xml::Writer;
use quick_xml::events::{BytesStart, Event};
use serde::Deserialize;

use super::xml::XmlElement;

/// A complete libvirt domain document. XML outside the replaced elements is
/// preserved verbatim, including unknown elements, namespaces, and attributes.
/// Transformations consume the document and return a new one without I/O.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct DomainXml {
    xml: String,
}

impl FromStr for DomainXml {
    type Err = eyre::Report;

    fn from_str(xml: &str) -> Result<Self, Self::Err> {
        let model = Self {
            xml: xml.to_owned(),
        };
        model.root()?;
        Ok(model)
    }
}

impl DomainXml {
    fn root(&self) -> eyre::Result<XmlElement<'_>> {
        let mut elements = XmlElement::read_children(&self.xml, 0..self.xml.len())?;
        eyre::ensure!(
            elements.len() == 1 && elements[0].is_named("domain"),
            "libvirt XML must contain one domain element"
        );
        Ok(elements.remove(0))
    }

    /// Replaces only direct boot children of the domain's OS element.
    /// Returns an error when the OS element is missing or ambiguous.
    pub(super) fn with_boot_order(self, devices: &[&str]) -> eyre::Result<Self> {
        let edit = self
            .root()?
            .child("os")?
            .replace_children("boot", &boot_elements(devices)?)?;
        Ok(Self {
            xml: edit.apply(self.xml),
        })
    }

    /// Reads only direct domain disk devices; unknown XML remains untouched.
    pub(super) fn disks(&self) -> eyre::Result<Vec<DomainDisk>> {
        let Some(devices) = self.root()?.optional_child("devices")? else {
            return Ok(Vec::new());
        };
        devices
            .children()?
            .into_iter()
            .filter(|element| element.is_named("disk"))
            .map(|element| {
                quick_xml::de::from_str(element.as_xml())
                    .context("could not parse libvirt disk XML")
            })
            .collect()
    }

    pub(super) fn into_xml(self) -> String {
        self.xml
    }
}

/// The disk identity needed by media ownership checks. This is a read-only
/// projection; it is never serialized over the original domain document.
#[derive(Debug, Deserialize)]
pub(super) struct DomainDisk {
    #[serde(rename = "@device")]
    pub(super) device: Option<String>,
    pub(super) target: Option<DiskTarget>,
    pub(super) alias: Option<DiskAlias>,
}

#[derive(Debug, Deserialize)]
pub(super) struct DiskTarget {
    #[serde(rename = "@dev")]
    pub(super) dev: Option<String>,
    #[serde(rename = "@bus")]
    pub(super) bus: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct DiskAlias {
    #[serde(rename = "@name")]
    pub(super) name: Option<String>,
}

fn boot_elements(devices: &[&str]) -> eyre::Result<String> {
    let mut writer = Writer::new(Vec::new());
    for device in devices {
        let mut boot = BytesStart::new("boot");
        boot.push_attribute(("dev", *device));
        writer
            .write_event(Event::Empty(boot))
            .context("could not write domain boot order")?;
    }
    String::from_utf8(writer.into_inner()).context("generated boot order is invalid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_only_domain_boot_order_and_preserves_unrelated_xml() {
        let xml = r#"<?xml version="1.0"?>
<domain type='kvm' xmlns:custom='urn:test'><name>test &amp; vm</name>
<metadata><custom:os custom:key='value'><boot dev='network'/></custom:os></metadata>
<os firmware='efi'><type arch='x86_64'>hvm</type><boot dev='hd'></boot><boot dev='network'/><!--keep--><loader readonly='yes'>/OVMF.fd</loader></os>
<devices><disk device='disk'><target dev='vda'/><boot order='1'/></disk></devices></domain>"#;
        let actual = xml
            .parse::<DomainXml>()
            .unwrap()
            .with_boot_order(&["cdrom", "hd"])
            .unwrap()
            .into_xml();
        let expected = xml
            .replace("<boot dev='hd'></boot><boot dev='network'/>", "")
            .replace("</os>", "<boot dev=\"cdrom\"/><boot dev=\"hd\"/></os>");
        assert_eq!(actual, expected);
    }

    #[test]
    fn consuming_transformations_can_be_chained() {
        for (name, xml) in [
            (
                "paired OS element",
                "<domain><os><boot dev='hd'/></os><devices/></domain>",
            ),
            ("empty OS element", "<domain><os/><devices/></domain>"),
        ] {
            let actual = xml
                .parse::<DomainXml>()
                .unwrap()
                .with_boot_order(&["cdrom", "hd"])
                .unwrap()
                .with_boot_order(&["network", "hd"])
                .unwrap()
                .into_xml();
            assert_eq!(
                actual,
                "<domain><os><boot dev=\"network\"/><boot dev=\"hd\"/></os><devices/></domain>",
                "{name}"
            );
        }
    }

    #[test]
    fn rejects_invalid_documents_and_missing_or_ambiguous_os() {
        for xml in ["<domain><os>", "<other/>", "<domain/><domain/>"] {
            assert!(xml.parse::<DomainXml>().is_err(), "{xml}");
        }
        for xml in ["<domain/>", "<domain><os/><os/></domain>"] {
            assert!(
                xml.parse::<DomainXml>()
                    .unwrap()
                    .with_boot_order(&["hd"])
                    .is_err(),
                "{xml}"
            );
        }
    }
    #[test]
    fn reads_direct_disks_and_decodes_attributes_without_rewriting_the_document() {
        let xml = "<domain><metadata><disk device='disk'><target dev='sdb'/></disk></metadata><devices><disk device='cdrom'><target dev='sdb' bus='sata'/><alias name='ua-bmc-mock-vmedia-C&#100;'/><source file='/tmp/media.iso'/></disk><interface/></devices></domain>";
        let domain: DomainXml = xml.parse().unwrap();
        let disks = domain.disks().unwrap();
        assert_eq!(disks.len(), 1);
        assert_eq!(
            disks[0].alias.as_ref().unwrap().name.as_deref(),
            Some("ua-bmc-mock-vmedia-Cd")
        );
        assert_eq!(domain.into_xml(), xml);
        assert!(
            "<domain/>"
                .parse::<DomainXml>()
                .unwrap()
                .disks()
                .unwrap()
                .is_empty()
        );
        for invalid in [
            "<domain><devices/><devices/></domain>",
            "<domain><devices><disk><target dev='sdb'/><target dev='sdc'/></disk></devices></domain>",
        ] {
            assert!(
                invalid.parse::<DomainXml>().unwrap().disks().is_err(),
                "{invalid}"
            );
        }
    }
}
