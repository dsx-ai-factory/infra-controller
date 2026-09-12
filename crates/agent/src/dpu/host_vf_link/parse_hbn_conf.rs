//! Parses host VF mappings from the contents of the `hbn.conf` file.

use std::collections::HashMap;

use eyre::WrapErr;

pub(super) const LINK_PROPAGATION_SECTION: &str = "LINK_PROPAGATION";
pub(super) const HBN_CONF_PATH: &str = "/etc/mellanox/hbn.conf";

#[derive(Default)]
struct SectionSplitState<'a> {
    next_line_start: usize,
    section_name: Option<&'a str>,
    section_contents_start: usize,
}

impl<'a> SectionSplitState<'a> {
    fn consume(&mut self, contents: &'a str, line: Option<&'a str>) -> Option<(&'a str, &'a str)> {
        let Some(line) = line else {
            return self.finish_section(contents, contents.len());
        };

        let line_start = self.next_line_start;
        self.next_line_start += line.len();

        let name = parse_section_header(line)?;

        let completed_section = self.finish_section(contents, line_start);
        self.section_name = Some(name);
        self.section_contents_start = self.next_line_start;
        completed_section
    }

    fn finish_section(
        &mut self,
        contents: &'a str,
        contents_end: usize,
    ) -> Option<(&'a str, &'a str)> {
        self.section_name
            .take()
            .map(|name| (name, &contents[self.section_contents_start..contents_end]))
    }
}

fn parse_section_header(line: &str) -> Option<&str> {
    line.trim().strip_prefix('[')?.strip_suffix(']')
}

fn split_sections(contents: &str) -> impl Iterator<Item = (&str, &str)> {
    contents
        .split_inclusive('\n')
        .map(Some)
        // This is a sentinel to stand in for EOF.
        .chain(std::iter::once(None))
        .scan(SectionSplitState::default(), move |state, line| {
            Some(state.consume(contents, line))
        })
        .flatten()
}

fn get_link_propagation_section(contents: &str) -> eyre::Result<&str> {
    let mut matching_sections = split_sections(contents)
        .filter_map(|(name, contents)| (name == LINK_PROPAGATION_SECTION).then_some(contents));
    let section = matching_sections
        .next()
        .ok_or_else(|| eyre::eyre!("missing [{LINK_PROPAGATION_SECTION}] section"))?;

    if matching_sections.next().is_some() {
        eyre::bail!("duplicate [{LINK_PROPAGATION_SECTION}] section");
    }

    Ok(section)
}

fn parse_mapping_line(line: &str) -> eyre::Result<(&str, &str)> {
    let (source, destination) = line
        .split_once(':')
        .ok_or_else(|| eyre::eyre!("malformed link propagation mapping {line:?}: missing ':'"))?;
    let source = source.trim();
    let destination = destination.trim();

    if source.is_empty() {
        eyre::bail!("malformed link propagation mapping {line:?}: empty source");
    }
    if destination.is_empty() {
        eyre::bail!("malformed link propagation mapping {line:?}: empty destination");
    }
    if destination.contains(':') {
        eyre::bail!("malformed link propagation mapping {line:?}: expected one ':'");
    }

    Ok((source, destination))
}

fn get_vf_id(source: &str) -> eyre::Result<Option<u32>> {
    let Some(vf_id) = source.strip_prefix("pf0vf") else {
        return Ok(None);
    };

    if vf_id.is_empty() || !vf_id.bytes().all(|byte| byte.is_ascii_digit()) {
        eyre::bail!("malformed host VF representor source {source:?}");
    }

    vf_id
        .parse::<u32>()
        .map(Some)
        .wrap_err_with(|| format!("parsing VF ID from host representor {source:?}"))
}

/// Parses host VF IDs and representor names from the HBN link-propagation configuration.
pub(super) fn get_hbn_vf_mapping(contents: &str) -> eyre::Result<HashMap<u32, String>> {
    let section = get_link_propagation_section(contents)?;
    let mut vf_mapping = HashMap::new();

    for line in section.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }

        let (source, _) = parse_mapping_line(line)?;
        let Some(vf_id) = get_vf_id(source)? else {
            continue;
        };
        if vf_mapping.insert(vf_id, source.to_string()).is_some() {
            eyre::bail!("duplicate host VF numeric ID {vf_id}");
        }
    }

    Ok(vf_mapping)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn extracts_link_propagation_section() {
        scenarios!(run = |contents| get_link_propagation_section(contents)
            .map(str::trim)
            .map_err(|error| error.to_string());
            "valid sections" {
                "[BEFORE]\nignored\n[LINK_PROPAGATION]\npf0vf1:rep1\n[AFTER]\nignored"
                    => Yields("pf0vf1:rep1"),
                "[BEFORE]\nignored\n[LINK_PROPAGATION]\npf0vf2:rep2"
                    => Yields("pf0vf2:rep2"),
                "[LINK_PROPAGATION]\n[NEXT]" => Yields(""),
            }

            "invalid sections" {
                "[OTHER]\npf0vf1:rep1" => Fails,
                "[link_propagation]\npf0vf1:rep1" => Fails,
                "[LINK_PROPAGATION]\npf0vf1:rep1\n[OTHER]\nignored\n[LINK_PROPAGATION]"
                    => Fails,
            }
        );
    }

    #[test]
    fn parses_mapping_lines() {
        scenarios!(run = |line| parse_mapping_line(line).map_err(|error| error.to_string());
            "valid mappings" {
                "pf0vf1:pf0vf1_if_r" => Yields(("pf0vf1", "pf0vf1_if_r")),
                " pf0vf2 : rep2 " => Yields(("pf0vf2", "rep2")),
            }

            "invalid mappings" {
                "pf0vf1" => Fails,
                ":rep1" => Fails,
                "pf0vf1:" => Fails,
                "pf0vf1:rep1:extra" => Fails,
            }
        );
    }

    #[test]
    fn recognizes_host_vf_sources() {
        scenarios!(run = |source| get_vf_id(source).map_err(|error| error.to_string());
            "host VFs" {
                "pf0vf0" => Yields(Some(0)),
                "pf0vf4294967295" => Yields(Some(u32::MAX)),
            }

            "unrelated representors" {
                "p0" => Yields(None),
                "pf0hpf" => Yields(None),
                "pf1vf3" => Yields(None),
            }

            "malformed host VFs" {
                "pf0vf" => Fails,
                "pf0vfone" => Fails,
                "pf0vf4294967296" => Fails,
            }
        );
    }

    #[test]
    fn parses_host_vf_mapping() {
        scenarios!(run = |contents| get_hbn_vf_mapping(contents).map_err(|error| error.to_string());
            "valid configuration" {
                "[GENERAL]\nignored=value\n\
                 [LINK_PROPAGATION]\n\
                 # HBN-owned host VFs\n\
                 pf0vf1:pf0vf1_if_r\n\
                 ; preserve the configured source name\n\
                 pf0vf07:pf0vf7_if_r\n\
                 p0:p0_if_r\n\
                 pf0hpf:pf0hpf_if_r\n\
                 [OTHER]\n\
                 pf0vf99:ignored"
                    => Yields(HashMap::from([
                        (1, "pf0vf1".to_string()),
                        (7, "pf0vf07".to_string()),
                    ])),
            }

            "invalid configuration" {
                "[LINK_PROPAGATION]\npf0vf1:rep1\npf0vf01:rep01" => Fails,
                "[LINK_PROPAGATION]\npf0vfbad:rep" => Fails,
            }
        );
    }
}
