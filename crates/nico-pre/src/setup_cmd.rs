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

//! `setup.command` and the `setup.env` variables that select components, resolved the way
//! `setup.sh` resolves them. One parser, shared by the config checks and the derived-decision
//! readers, so they cannot disagree on what the command installs.

use crate::sitedef::{EnvValue, Setup};

/// `setup.command` as `setup.sh` would parse it: the switches, and the paths given
/// to the flags that take one.
#[allow(
    dead_code,
    reason = "the gate reads the component switches; the config checklist reads the paths too"
)]
pub(crate) struct SetupCommand {
    pub(crate) switches: Vec<String>,
    pub(crate) core_values: Option<String>,
    pub(crate) metallb_config: Option<String>,
    pub(crate) site_overlay: Option<String>,
    /// `setup.env` entries that select components when no switch does.
    env_skip_dpf: bool,
    env_skip_rms: bool,
    env_observability: bool,
    env_contour: bool,
    /// An unterminated quote in `setup.command`; the shell would refuse it too.
    pub(crate) quoting_error: Option<String>,
    /// A `NICO_INSTALL_DPF` or `NICO_INSTALL_RMS` value setup.sh rejects, when no skip
    /// variable or switch overrides it; the message is the one setup.sh prints.
    pub(crate) env_error: Option<String>,
    /// Whether any simple command on the line runs `setup.sh`; when none does (a developer
    /// site deployed by other scripts) the command decides nothing.
    runs_setup_sh: bool,
}

/// Splits a command line the way a POSIX shell does for a simple command: whitespace
/// separates words, single quotes keep everything, double quotes keep everything but
/// allow backslash escapes, a backslash escapes the next character, a backslash before
/// a newline joins the lines, and an unquoted `#` at a word boundary starts a comment.
/// Unquoted control operators and parentheses are words of their own, while a `$(…)`
/// substitution stays inside its word; redirections and their targets are dropped. No expansion, no execution. An unterminated quote or a
/// redirection without a target returns what was read so far and the error.
pub(crate) fn shell_words(line: &str) -> (Vec<String>, Option<String>) {
    /// Reads a parenthesised body verbatim into `word` up to and including the `)` that
    /// closes it, honouring nested parentheses, quotes and backslash escapes, as in
    /// `$(…)`, `>(…)` and `<(…)`. Returns whether the body was closed.
    fn read_balanced(
        chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
        word: &mut String,
    ) -> bool {
        let mut depth = 1usize;
        while let Some(q) = chars.next() {
            word.push(q);
            match q {
                '\\' => word.extend(chars.next()),
                '\'' => {
                    for n in chars.by_ref() {
                        word.push(n);
                        if n == '\'' {
                            break;
                        }
                    }
                }
                '"' => {
                    while let Some(n) = chars.next() {
                        word.push(n);
                        if n == '\\' {
                            word.extend(chars.next());
                        } else if n == '"' {
                            break;
                        }
                    }
                }
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    let mut error = None;
    // The word after a redirection operator is its target, not an argument.
    let mut drop_next_word = false;
    // Whether any part of the current word was quoted or escaped: `"2">f` is an argument
    // `2` and a redirection, not descriptor 2 redirected.
    let mut word_quoted = false;
    let finish =
        |words: &mut Vec<String>, word: &mut String, drop: &mut bool, quoted: &mut bool| {
            *quoted = false;
            if *drop {
                word.clear();
                *drop = false;
            } else {
                words.push(std::mem::take(word));
            }
        };
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    finish(&mut words, &mut word, &mut drop_next_word, &mut word_quoted);
                    in_word = false;
                }
                // An unescaped newline ends the command like `;` does.
                if c == '\n' && !words.is_empty() && words.last().map(String::as_str) != Some(";") {
                    words.push(";".to_string());
                }
            }
            '\\' => match chars.next() {
                // A continuation joins the lines; it is not part of any word.
                Some('\n') => {}
                Some(next) => {
                    in_word = true;
                    word_quoted = true;
                    word.push(next);
                }
                None => in_word = true,
            },
            // `>(…)` and `<(…)` are process substitutions: one word, kept verbatim, dropped
            // like any other target when a redirection precedes it (`> >(tee log)`).
            '<' | '>' if chars.peek() == Some(&'(') => {
                if in_word {
                    finish(&mut words, &mut word, &mut drop_next_word, &mut word_quoted);
                }
                chars.next();
                word.push(c);
                word.push('(');
                if !read_balanced(&mut chars, &mut word) {
                    error.get_or_insert_with(|| "unterminated process substitution".to_string());
                }
                in_word = true;
                word_quoted = true;
            }
            // Unquoted redirections (`>file`, `2>&1`, `&>file`, `<file`) end the word before
            // them and swallow their target; a leading descriptor number belongs to them.
            '<' | '>' => {
                if in_word && (word_quoted || !word.chars().all(|c| c.is_ascii_digit())) {
                    finish(&mut words, &mut word, &mut drop_next_word, &mut word_quoted);
                }
                word.clear();
                word_quoted = false;
                in_word = false;
                while matches!(chars.peek(), Some('>') | Some('<')) {
                    chars.next();
                }
                // `>&2` duplicates a descriptor and names no file; `>& file` does.
                if chars.peek() == Some(&'&') {
                    chars.next();
                    if chars
                        .peek()
                        .is_some_and(|c| c.is_ascii_digit() || *c == '-')
                    {
                        while chars
                            .peek()
                            .is_some_and(|c| c.is_ascii_digit() || *c == '-')
                        {
                            chars.next();
                        }
                        continue;
                    }
                }
                drop_next_word = true;
            }
            '&' if chars.peek() == Some(&'>') => {
                if in_word {
                    finish(&mut words, &mut word, &mut drop_next_word, &mut word_quoted);
                    in_word = false;
                }
                while matches!(chars.peek(), Some('>')) {
                    chars.next();
                }
                drop_next_word = true;
            }
            // `$(…)` is part of its word, nesting included; it is kept verbatim, unexpanded.
            '(' if in_word && word.ends_with('$') => {
                word.push('(');
                if !read_balanced(&mut chars, &mut word) {
                    error.get_or_insert_with(|| "unterminated command substitution".to_string());
                }
            }
            // Unquoted control operators and parentheses are words of their own, attached
            // or not (`--skip-rms;` is the switch and then `;`, `--skip-rms)` likewise).
            ';' | '&' | '|' | '(' | ')' => {
                if in_word {
                    finish(&mut words, &mut word, &mut drop_next_word, &mut word_quoted);
                    in_word = false;
                }
                // A redirection with no target before the operator is the shell's error;
                // the next command's first word is not it.
                if drop_next_word {
                    error.get_or_insert_with(|| "redirection without a target".to_string());
                    drop_next_word = false;
                }
                let mut op = String::from(c);
                if (c == '&' || c == '|') && chars.peek() == Some(&c) {
                    op.push(c);
                    chars.next();
                }
                words.push(op);
            }
            // An unquoted `#` at a word boundary comments out the rest of its line only;
            // the line's end still ends the command.
            '#' if !in_word => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        if !words.is_empty() && words.last().map(String::as_str) != Some(";") {
                            words.push(";".to_string());
                        }
                        break;
                    }
                }
            }
            '\'' => {
                in_word = true;
                word_quoted = true;
                let mut closed = false;
                for q in chars.by_ref() {
                    if q == '\'' {
                        closed = true;
                        break;
                    }
                    word.push(q);
                }
                if !closed {
                    error = Some("unterminated single quote".to_string());
                }
            }
            '"' => {
                in_word = true;
                word_quoted = true;
                let mut closed = false;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => {
                            closed = true;
                            break;
                        }
                        // Inside double quotes a backslash escapes only `$`, a backtick,
                        // `"`, `\` and the newline; elsewhere it is kept as text.
                        '\\' => match chars.next() {
                            Some('\n') => {}
                            Some(next @ ('$' | '`' | '"' | '\\')) => word.push(next),
                            Some(next) => {
                                word.push('\\');
                                word.push(next);
                            }
                            None => word.push('\\'),
                        },
                        other => word.push(other),
                    }
                }
                if !closed {
                    error = Some("unterminated double quote".to_string());
                }
            }
            other => {
                in_word = true;
                word.push(other);
            }
        }
    }
    if in_word {
        finish(&mut words, &mut word, &mut drop_next_word, &mut word_quoted);
    }
    if drop_next_word {
        error.get_or_insert_with(|| "redirection without a target".to_string());
    }
    (words, error)
}

#[allow(
    dead_code,
    reason = "the gate reads the component switches; the config checklist reads the rest"
)]
impl SetupCommand {
    /// The command as `setup.sh` reads it, with `setup.env` for the variables that select
    /// components (`NICO_SKIP_DPF`, `NICO_INSTALL_DPF`, `NICO_SKIP_RMS`, `NICO_INSTALL_RMS`,
    /// `WITH_OBSERVABILITY`, `NICO_INSTALL_CONTOUR`); a switch wins over its variable.
    pub(crate) fn parse(setup: &Setup) -> Self {
        let (words, quoting_error) = shell_words(&setup.command);
        // Leading `NAME=value` words are assignments setup.sh sees in its environment, so
        // they decide a component exactly as the same name in `setup.env` would.
        let is_assignment = |w: &str| {
            w.split_once('=').is_some_and(|(name, _)| {
                !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !name.starts_with(|c: char| c.is_ascii_digit())
            })
        };
        let is_operator = |w: &str| matches!(w, "&&" | "||" | ";" | "|" | "&" | "(" | ")");
        fn basename(p: &str) -> &str {
            p.rsplit('/').next().unwrap_or(p)
        }
        // The switches belong to the simple command that runs setup.sh. The line may `cd`
        // first, or reach the script through `env`, `sudo`, `nohup` or an interpreter with
        // options of their own, so the script is found by name within its command, and
        // every `NAME=value` word before it is an assignment it will see. A command runs
        // the script when only wrappers and their arguments stand before it; `test -x
        // ./setup.sh`, `rm ./setup.sh` or `env test -x ./setup.sh` merely name it. When no command on the
        // line qualifies (an unlisted wrapper), the first one naming it is taken, so a
        // line that names setup.sh is never silently ignored.
        const WRAPPERS: [&str; 16] = [
            "env", "sudo", "doas", "nohup", "time", "nice", "ionice", "timeout", "stdbuf",
            "setsid", "exec", "command", "bash", "sh", "dash", "zsh",
        ];
        // A command invokes the script when only wrappers, their options and operands
        // stand before it. An option's operand follows the option (`-o pipefail`, `-k 10`,
        // `-u name`); `timeout` also takes a bare duration. `env test -x ./setup.sh`
        // fails here at `test`, so a wrapped guard is still a reference.
        let is_invocation = |before: &[&str]| {
            let mut after_option = false;
            let mut positional = 0usize;
            for w in before {
                if WRAPPERS.contains(&basename(w)) {
                    positional = usize::from(basename(w) == "timeout");
                    after_option = false;
                } else if w.starts_with(['-', '+']) {
                    after_option = true;
                } else if after_option {
                    after_option = false;
                } else if positional > 0 {
                    positional -= 1;
                } else {
                    return false;
                }
            }
            true
        };
        /// A simple command on the line that names setup.sh.
        struct Naming<'a> {
            /// Assignments before `env` (or all of them without one): the environment the
            /// command inherits, which `env -u` and `env -i` can take away.
            inherited: Vec<(&'a str, &'a str)>,
            /// Assignments on the `env` command line itself: added after the removals.
            own: Vec<(&'a str, &'a str)>,
            args: Vec<&'a str>,
            invokes: bool,
            /// Variables `env -u` removes, and whether `env -i` clears the inherited ones.
            unset: Vec<&'a str>,
            clear_env: bool,
        }
        // `bash -c '…'` in command position runs the string as a command line of its own:
        // its words replace the interpreter's, every command inside inherits what stood
        // before the interpreter (assignments, `env` controls, wrappers), and anything
        // after the string is `$0` and positional parameters, not switches. A `bash`
        // that is another command's argument is left alone.
        let mut quoting_error = quoting_error;
        let mut expanded: Vec<String> = Vec::with_capacity(words.len());
        let mut i = 0;
        while i < words.len() {
            let start = expanded
                .iter()
                .rposition(|w| is_operator(w))
                .map_or(0, |p| p + 1);
            let before: Vec<&str> = expanded[start..]
                .iter()
                .map(String::as_str)
                .filter(|w| !is_assignment(w))
                .collect();
            let interpreter = matches!(basename(&words[i]), "bash" | "sh" | "dash" | "zsh");
            if interpreter
                && is_invocation(&before)
                && words.get(i + 1).map(String::as_str) == Some("-c")
                && let Some(script) = words.get(i + 2)
            {
                let (inner, err) = shell_words(script);
                if quoting_error.is_none() {
                    quoting_error = err;
                }
                let outer: Vec<String> = expanded.drain(start..).collect();
                let mut at_segment_start = true;
                for w in inner {
                    if at_segment_start && !is_operator(&w) {
                        expanded.extend(outer.iter().cloned());
                    }
                    at_segment_start = is_operator(&w);
                    expanded.push(w);
                }
                i += 3;
                while i < words.len() && !is_operator(&words[i]) {
                    i += 1;
                }
            } else {
                expanded.push(words[i].clone());
                i += 1;
            }
        }
        let words = expanded;
        let naming: Vec<Naming> = words
            .split(|w| is_operator(w))
            .filter_map(|seg| {
                let at = seg
                    .iter()
                    .position(|w| !is_assignment(w) && basename(w) == "setup.sh")?;
                let env_at = seg[..at]
                    .iter()
                    .position(|w| !is_assignment(w) && basename(w) == "env")
                    .unwrap_or(at);
                let inherited: Vec<(&str, &str)> = seg[..env_at]
                    .iter()
                    .filter(|w| is_assignment(w))
                    .filter_map(|w| w.split_once('='))
                    .collect();
                let own: Vec<(&str, &str)> = seg[env_at..at]
                    .iter()
                    .filter(|w| is_assignment(w))
                    .filter_map(|w| w.split_once('='))
                    .collect();
                let before: Vec<&str> = seg[..at]
                    .iter()
                    .map(String::as_str)
                    .filter(|w| !is_assignment(w))
                    .collect();
                let args: Vec<&str> = seg[at + 1..].iter().map(String::as_str).collect();
                // `env -u NAME` and `env -i` change what setup.sh inherits from `setup.env`.
                let mut unset = Vec::new();
                let mut clear_env = false;
                if let Some(env_at) = before.iter().position(|w| basename(w) == "env") {
                    let mut it = before[env_at + 1..].iter();
                    while let Some(w) = it.next() {
                        match *w {
                            "-i" | "--ignore-environment" => clear_env = true,
                            "-u" | "--unset" => unset.extend(it.next()),
                            _ if w.starts_with("--unset=") => unset.push(&w["--unset=".len()..]),
                            _ if w.starts_with("-u") && w.len() > 2 => unset.push(&w[2..]),
                            _ => {}
                        }
                    }
                }
                Some(Naming {
                    inherited,
                    own,
                    args,
                    invokes: is_invocation(&before),
                    unset,
                    clear_env,
                })
            })
            .collect();
        let segment = naming
            .iter()
            .position(|n| n.invokes)
            .or_else(|| (!naming.is_empty()).then_some(0))
            .map(|i| &naming[i]);
        let runs_setup_sh = segment.is_some();
        let words: Vec<&str> = segment.map(|n| n.args.clone()).unwrap_or_default();
        // A variable as setup.sh will see it: an assignment on the line wins; otherwise
        // `setup.env`, unless `env -u` removed it or `env -i` cleared the environment.
        // A variable as setup.sh will see it: an assignment on the `env` line itself wins;
        // then, unless `env -u` removed it or `env -i` cleared the environment, an
        // assignment before `env` or the value in `setup.env`.
        fn last<'a>(list: &[(&'a str, &'a str)], name: &str) -> Option<&'a str> {
            list.iter().rev().find(|(k, _)| *k == name).map(|(_, v)| *v)
        }
        let plain = |name: &str| {
            let n = segment?;
            last(&n.own, name).or_else(|| {
                if n.clear_env || n.unset.contains(&name) {
                    return None;
                }
                last(&n.inherited, name).or_else(|| match setup.env.get(name) {
                    Some(EnvValue::Plain(v)) => Some(v.as_str()),
                    _ => None,
                })
            })
        };
        let mut c = SetupCommand {
            switches: Vec::new(),
            core_values: None,
            metallb_config: None,
            site_overlay: None,
            env_skip_dpf: plain("NICO_SKIP_DPF") == Some("true")
                || plain("NICO_INSTALL_DPF") == Some("false"),
            env_skip_rms: plain("NICO_SKIP_RMS") == Some("true")
                || plain("NICO_INSTALL_RMS") == Some("false"),
            env_observability: plain("WITH_OBSERVABILITY") == Some("true"),
            env_contour: plain("NICO_INSTALL_CONTOUR") == Some("true"),
            quoting_error,
            env_error: None,
            runs_setup_sh,
        };
        // setup.sh starts from NICO_INSTALL_*, lets NICO_SKIP_*=true and then the switches
        // override it, and exits unless the result is exactly `true` or `false`.
        let invalid_install = |install: &str, skip: &str, skip_sw: &str, install_sw: &str| {
            let overridden = plain(skip) == Some("true")
                || words.iter().any(|w| *w == skip_sw || *w == install_sw);
            match plain(install) {
                // `${NICO_INSTALL_DPF:-true}`: setup.sh takes an empty value as unset.
                Some(v) if !v.is_empty() && !overridden && v != "true" && v != "false" => {
                    Some(format!("{install} must be true or false (got '{v}')"))
                }
                _ => None,
            }
        };
        if runs_setup_sh {
            c.env_error = invalid_install(
                "NICO_INSTALL_DPF",
                "NICO_SKIP_DPF",
                "--skip-dpf",
                "--install-dpf",
            )
            .or_else(|| {
                invalid_install(
                    "NICO_INSTALL_RMS",
                    "NICO_SKIP_RMS",
                    "--skip-rms",
                    "--install-rms",
                )
            });
        }
        let mut i = 0;
        while i < words.len() {
            let w = words[i];
            let value = words.get(i + 1).map(|v| v.to_string());
            let takes_value = value.is_some();
            match w {
                "--core-values" => {
                    c.core_values = value;
                    i += usize::from(takes_value);
                }
                "--metallb-config" => {
                    c.metallb_config = value;
                    i += usize::from(takes_value);
                }
                "--site-overlay" => {
                    c.site_overlay = value;
                    i += usize::from(takes_value);
                }
                "--flow-values" => i += usize::from(takes_value),
                _ if w.starts_with("--") => c.switches.push(w.to_string()),
                _ => {}
            }
            i += 1;
        }
        c
    }

    pub(crate) fn has(&self, switch: &str) -> bool {
        self.switches.iter().any(|s| s == switch)
    }

    /// False when no simple command on the line runs `setup.sh`; the component
    /// readers then describe setup.sh's defaults, not a decision the provider made.
    pub(crate) fn runs_setup_sh(&self) -> bool {
        self.runs_setup_sh
    }

    /// `setup.sh` installs DPF and RMS unless a switch or a variable says otherwise.
    /// The last of two opposing switches wins, as `setup.sh` applies them in argument
    /// order; the variables decide only when neither switch is given.
    fn last_of(&self, skip: &str, install: &str, env_skips: bool) -> bool {
        match self
            .switches
            .iter()
            .rev()
            .find(|s| s.as_str() == skip || s.as_str() == install)
        {
            Some(s) => s == install,
            None => !env_skips,
        }
    }
    pub(crate) fn installs_dpf(&self) -> bool {
        self.last_of("--skip-dpf", "--install-dpf", self.env_skip_dpf)
    }
    pub(crate) fn installs_rms(&self) -> bool {
        self.last_of("--skip-rms", "--install-rms", self.env_skip_rms)
    }
    pub(crate) fn with_observability(&self) -> bool {
        self.has("--with-observability") || self.env_observability
    }
    pub(crate) fn installs_contour(&self) -> bool {
        self.has("--install-contour") || self.env_contour
    }
    pub(crate) fn installs_rest(&self) -> bool {
        !self.has("--skip-rest")
    }
    pub(crate) fn installs_core(&self) -> bool {
        !self.has("--skip-core")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(line: &str) -> SetupCommand {
        let setup = Setup {
            command: line.to_string(),
            env: Default::default(),
        };
        SetupCommand::parse(&setup)
    }

    #[test]
    fn quoted_switches_and_paths_parse_as_the_shell_would() {
        let c = cmd(r#"./setup.sh "--skip-dpf" --core-values 'values/my site.yaml' --skip-rms"#);
        assert!(!c.installs_dpf(), "a quoted switch is still the switch");
        assert!(!c.installs_rms());
        assert_eq!(c.core_values.as_deref(), Some("values/my site.yaml"));
        assert!(c.quoting_error.is_none());
        let c = cmd(r#"./setup.sh --metallb-config values/a\ b.yaml"#);
        assert_eq!(c.metallb_config.as_deref(), Some("values/a b.yaml"));
    }

    #[test]
    fn comments_and_continuations_read_as_the_shell_reads_them() {
        let c = cmd("./setup.sh --skip-rms # --skip-dpf");
        assert!(c.installs_dpf(), "a commented-out switch is not a switch");
        assert!(!c.installs_rms());
        let c =
            cmd("./setup.sh \\\n  --with-observability \\\n  --core-values values/nico-core.yaml");
        assert!(c.with_observability(), "a continuation joins the lines");
        assert_eq!(c.core_values.as_deref(), Some("values/nico-core.yaml"));
        let c = cmd(r##"./setup.sh --site-overlay "over#lay""##);
        assert_eq!(
            c.site_overlay.as_deref(),
            Some("over#lay"),
            "a quoted # is text"
        );
    }

    #[test]
    fn a_leading_comment_and_leading_assignments_are_read() {
        let c = cmd("# deployment options\n./setup.sh --skip-dpf --skip-rms");
        assert!(
            !c.installs_dpf(),
            "the command after a comment line still counts"
        );
        assert!(!c.installs_rms());
        let c = cmd("NICO_SKIP_DPF=true WITH_OBSERVABILITY=true ./setup.sh --skip-rms");
        assert!(
            !c.installs_dpf(),
            "a leading assignment is setup.sh's environment"
        );
        assert!(c.with_observability());
        assert!(!c.switches.iter().any(|w| w.contains('=')));
    }

    #[test]
    fn switches_after_a_shell_operator_belong_to_another_command() {
        let c = cmd("./setup.sh --skip-rms && echo --skip-dpf");
        assert!(c.installs_dpf(), "--skip-dpf is echo's argument");
        assert!(!c.installs_rms());
        let c = cmd("./setup.sh ; ./other.sh --skip-dpf");
        assert!(c.installs_dpf());
        let c = cmd("./setup.sh --skip-rms; echo --skip-dpf");
        assert!(c.installs_dpf(), "an attached `;` still ends the command");
        assert!(!c.installs_rms());
    }

    #[test]
    fn an_unescaped_newline_ends_the_command() {
        let c = cmd("./setup.sh --skip-rms\necho --skip-dpf");
        assert!(
            c.installs_dpf(),
            "--skip-dpf belongs to echo on the next line"
        );
        assert!(!c.installs_rms());
        let c = cmd("./setup.sh --skip-rms # note\necho --skip-dpf");
        assert!(c.installs_dpf(), "a comment does not join the lines");
        let c = cmd("# options\n\n./setup.sh --skip-dpf\n");
        assert!(
            !c.installs_dpf(),
            "leading comments and blank lines are skipped"
        );
    }

    #[test]
    fn an_env_prefix_and_an_operator_after_a_value_switch_are_read_as_the_shell_does() {
        let c = cmd("env NICO_SKIP_DPF=true ./setup.sh --skip-rms");
        assert!(
            !c.installs_dpf(),
            "`env NAME=value` sets setup.sh's environment"
        );
        let c = cmd("./setup.sh --flow-values ; echo --skip-dpf");
        assert!(
            c.installs_dpf(),
            "an operator is not a switch's value and ends the command"
        );
        assert!(c.core_values.is_none());
    }

    #[test]
    fn redirections_are_not_command_separators() {
        let c = cmd("./setup.sh 2>&1 --skip-rms");
        assert!(
            !c.installs_rms(),
            "2>&1 is a redirection inside the command"
        );
        let c = cmd("./setup.sh &> setup.log --skip-dpf");
        assert!(!c.installs_dpf(), "&> is a redirection too");
        let c = cmd("./setup.sh --skip-rms >out.txt & echo --skip-dpf");
        assert!(c.installs_dpf(), "a bare & still ends the command");
    }

    #[test]
    fn switches_come_from_the_simple_command_that_runs_setup_sh() {
        let c = cmd("cd helm-prereqs && ./setup.sh --skip-rms --skip-dpf");
        assert!(c.runs_setup_sh());
        assert!(!c.installs_rms() && !c.installs_dpf(), "after `cd … &&`");
        let c =
            cmd("cd helm-prereqs && NICO_SKIP_RMS=true bash -x ./setup.sh --core-values v.yaml");
        assert!(!c.installs_rms(), "an assignment before the interpreter");
        assert_eq!(c.core_values.as_deref(), Some("v.yaml"));
        let c = cmd("test -x ./setup.sh && ./setup.sh --skip-rms");
        assert!(
            !c.installs_rms(),
            "a guard naming the script is not the command that runs it"
        );
        let c = cmd("(cd helm-prereqs && ./setup.sh --skip-rms)");
        assert!(
            !c.installs_rms(),
            "a closing parenthesis is not part of the switch"
        );
        let c = cmd("bash -c './setup.sh --skip-dpf --skip-rms'");
        assert!(
            c.runs_setup_sh() && !c.installs_dpf() && !c.installs_rms(),
            "`bash -c` runs the string"
        );
        let c = cmd("NICO_SKIP_RMS=true bash -c 'env -u NICO_SKIP_RMS ./setup.sh --skip-dpf'");
        assert!(
            c.installs_rms() && !c.installs_dpf(),
            "an inner `env -u` removes an inherited assignment"
        );
        let c = cmd("./setup.sh --core-values $(printf '%s' 'site(.yaml') --skip-rms");
        assert_eq!(
            c.core_values.as_deref(),
            Some("$(printf '%s' 'site(.yaml')"),
            "quotes inside a substitution"
        );
        assert!(!c.installs_rms() && c.quoting_error.is_none());
        let c = cmd("./setup.sh > >(tee setup.log) --skip-rms");
        assert!(
            !c.installs_rms() && c.quoting_error.is_none(),
            "a process substitution is one target"
        );
        let c = cmd("NICO_SKIP_RMS=true bash -c 'cd helm-prereqs && ./setup.sh --skip-dpf'");
        assert!(
            !c.installs_rms() && !c.installs_dpf(),
            "the outer assignment reaches every embedded command"
        );
        let c = cmd(
            r"printf '%s\n' bash -c 'echo preview; ./setup.sh --skip-rms' && ./setup.sh --skip-dpf",
        );
        assert!(
            c.installs_rms() && !c.installs_dpf(),
            "`bash` as an argument is not expanded"
        );
        let c = cmd("./setup.sh --core-values $(pwd)/values.yaml --skip-rms");
        assert_eq!(
            c.core_values.as_deref(),
            Some("$(pwd)/values.yaml"),
            "a substitution stays in its word"
        );
        assert!(!c.installs_rms());
        let c = cmd("bash -c './setup.sh --skip-rms' bash --skip-dpf");
        assert!(
            c.installs_dpf(),
            "words after the `-c` string are positional parameters"
        );
        let c = cmd("env test -x ./setup.sh && ./setup.sh --skip-rms");
        assert!(!c.installs_rms(), "a wrapped guard is still a guard");
        let c = cmd("./setup.sh --skip-rms --core-values v.yaml && rm ./setup.sh");
        assert!(
            !c.installs_rms(),
            "a trailing reference does not replace the invocation"
        );
        assert_eq!(c.core_values.as_deref(), Some("v.yaml"));
        let c = cmd("caffeinate ./setup.sh --skip-rms");
        assert!(
            !c.installs_rms(),
            "an unlisted wrapper still reads the switches"
        );
        let c = cmd("timeout -k 10 300 ./setup.sh --skip-rms; ./setup.sh --skip-dpf");
        assert!(
            !c.installs_rms() && c.installs_dpf(),
            "a wrapper's operands do not hide the script"
        );
        let c = cmd("env NICO_SCRIPT=/opt/helpers/setup.sh NICO_SKIP_RMS=true ./setup.sh");
        assert!(
            c.runs_setup_sh() && !c.installs_rms(),
            "an assignment naming the script is not it"
        );
        let c = cmd("env -u BASH_ENV ./setup.sh --skip-dpf");
        assert!(!c.installs_dpf(), "`env` options do not hide the script");
        let c = cmd("bash -euo pipefail ./setup.sh --skip-dpf");
        assert!(
            !c.installs_dpf(),
            "combined interpreter options do not hide it either"
        );
        let c = cmd("sudo -E nohup ./setup.sh --skip-rms");
        assert!(!c.installs_rms(), "nor do wrappers");
        let c = cmd("NICO_SKIP_RMS=true env ./setup.sh --skip-dpf");
        assert!(
            c.runs_setup_sh(),
            "`env` after an assignment still runs setup.sh"
        );
        assert!(!c.installs_rms() && !c.installs_dpf());
        let c = cmd("bash -o pipefail ./setup.sh --skip-dpf");
        assert!(
            !c.installs_dpf(),
            "`-o pipefail` is one option, the script follows"
        );
        let c = cmd("sh -- ./setup.sh --skip-rms");
        assert!(!c.installs_rms(), "`--` ends the interpreter's options");
        let c = cmd("python3 tools/nico-dev/deploy-dev-nico.py --site dc1/dev1");
        assert!(!c.runs_setup_sh(), "another deployer decides nothing");
        assert!(c.switches.is_empty(), "{:?}", c.switches);
        let c = cmd("./setup.sh --skip-rms && echo --skip-dpf");
        assert!(
            !c.installs_rms() && c.installs_dpf(),
            "the next command's words are not switches"
        );
    }

    #[test]
    fn redirection_operators_and_their_targets_are_not_arguments() {
        let c = cmd("./setup.sh --skip-rms>setup.log");
        assert!(!c.installs_rms(), "the switch ends at the redirection");
        let c = cmd("./setup.sh --skip-rms > --skip-dpf");
        assert!(c.installs_dpf(), "the redirection target is not a switch");
        let c = cmd("./setup.sh >> 'set up.log' 2>&1 --core-values v.yaml");
        assert_eq!(c.core_values.as_deref(), Some("v.yaml"));
        let c = cmd("./setup.sh --skip-dpf 2>/dev/null");
        assert!(
            !c.installs_dpf(),
            "a descriptor number is part of the redirection"
        );
        let c = cmd(r#"./setup.sh --core-values "2">/dev/null --skip-rms"#);
        assert_eq!(
            c.core_values.as_deref(),
            Some("2"),
            "a quoted digit is an argument"
        );
        assert!(!c.installs_rms());
        let (_, err) = shell_words("./setup.sh --skip-rms >");
        assert_eq!(err.as_deref(), Some("redirection without a target"));
        let (words, err) = shell_words("./setup.sh > ; echo x");
        assert!(
            err.is_some(),
            "a dangling redirection before an operator is an error too"
        );
        assert_eq!(
            words,
            ["./setup.sh", ";", "echo", "x"],
            "an operator ends a dangling redirection"
        );
    }

    #[test]
    fn install_variables_must_be_true_or_false_unless_overridden() {
        let with_env = |command: &str, pairs: &[(&str, &str)]| {
            let setup = Setup {
                command: command.to_string(),
                env: pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), EnvValue::Plain(v.to_string())))
                    .collect(),
            };
            SetupCommand::parse(&setup)
        };
        let c = with_env("./setup.sh", &[("NICO_INSTALL_DPF", "yes")]);
        assert_eq!(
            c.env_error.as_deref(),
            Some("NICO_INSTALL_DPF must be true or false (got 'yes')")
        );
        let c = with_env("./setup.sh --skip-dpf", &[("NICO_INSTALL_DPF", "yes")]);
        assert!(c.env_error.is_none(), "a switch overrides the variable");
        let c = with_env(
            "./setup.sh",
            &[("NICO_INSTALL_DPF", "yes"), ("NICO_SKIP_DPF", "true")],
        );
        assert!(c.env_error.is_none(), "NICO_SKIP_DPF=true overrides it too");
        let c = with_env(
            "env -u NICO_SKIP_RMS ./setup.sh",
            &[("NICO_SKIP_RMS", "true")],
        );
        assert!(
            c.installs_rms(),
            "`env -u` removes the variable setup.sh would have seen"
        );
        let c = with_env("env -i ./setup.sh", &[("NICO_SKIP_DPF", "true")]);
        assert!(
            c.installs_dpf(),
            "`env -i` clears the inherited environment"
        );
        let c = with_env("env -i NICO_SKIP_DPF=true ./setup.sh", &[]);
        assert!(
            !c.installs_dpf(),
            "an assignment on the env line survives `-i`"
        );
        let c = with_env("./setup.sh", &[("NICO_INSTALL_DPF", "")]);
        assert!(
            c.env_error.is_none() && c.installs_dpf(),
            "an empty value is unset: the default"
        );
        let c = with_env("NICO_INSTALL_RMS=1 ./setup.sh", &[]);
        assert_eq!(
            c.env_error.as_deref(),
            Some("NICO_INSTALL_RMS must be true or false (got '1')")
        );
    }

    #[test]
    fn the_last_opposing_switch_wins() {
        let c = cmd("./setup.sh --skip-dpf --install-dpf");
        assert!(c.installs_dpf());
        let c = cmd("./setup.sh --install-rms --skip-rms");
        assert!(!c.installs_rms());
    }

    #[test]
    fn a_double_quoted_backslash_stays_unless_it_escapes() {
        let c = cmd(r#"./setup.sh --core-values "values/my\ site.yaml" --metallb-config "a\"b""#);
        assert_eq!(c.core_values.as_deref(), Some(r"values/my\ site.yaml"));
        assert_eq!(c.metallb_config.as_deref(), Some(r#"a"b"#));
    }

    #[test]
    fn an_unterminated_quote_is_reported() {
        let c = cmd(r#"./setup.sh "--skip-dpf"#);
        assert_eq!(
            c.quoting_error.as_deref(),
            Some("unterminated double quote")
        );
    }
}
