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
    /// What the shell would refuse in `setup.command`: an unterminated quote, backtick,
    /// command or process substitution, or a redirection without a target.
    pub(crate) parse_error: Option<String>,
    /// A `NICO_INSTALL_DPF` or `NICO_INSTALL_RMS` value setup.sh rejects, when no skip
    /// variable or switch overrides it; the message is the one setup.sh prints.
    pub(crate) env_error: Option<String>,
    /// A word setup.sh does not accept; it exits 1 on one, a misspelled switch included.
    pub(crate) switch_error: Option<String>,
    /// Words after `setup.sh` the shell would expand that cannot be read here (`$FLAGS`
    /// with a secret or unset variable, `$(…)`): whatever they add is not compared.
    pub(crate) unexpanded: Vec<String>,
    /// Whether any simple command on the line runs `setup.sh`; when none does (a developer
    /// site deployed by other scripts) the command decides nothing.
    runs_setup_sh: bool,
}

/// One token of a command line.
#[derive(Clone, Debug)]
pub(crate) enum Token {
    /// A word, quotes and escapes resolved; a quoted `;` or `(` is a word, not an operator.
    Word(String),
    /// An unquoted control operator or parenthesis: `;`, `&`, `|`, `&&`, `||`, `(`, `)`.
    Op(String),
    /// The environment an embedded command line (`bash -c '…'`) inherits from the command
    /// around the interpreter, resolved at that boundary and placed at the start of each
    /// embedded command; outermost layer first.
    Inherits(Vec<Layer>),
}

impl Token {
    /// The token's text; a word and an operator both have one.
    #[cfg(test)]
    fn text(&self) -> Option<&str> {
        match self {
            Token::Word(w) | Token::Op(w) => Some(w),
            Token::Inherits(_) => None,
        }
    }
    fn is_op(&self) -> bool {
        matches!(self, Token::Op(_))
    }
}

/// What the words before a command's program do to the environment it inherits:
/// assignments before `env` are in the inherited environment, `env -i` and `env -u` take
/// from it, assignments on the `env` line itself are added after that.
#[derive(Clone, Debug, Default)]
pub(crate) struct Layer {
    pre: Vec<(String, String)>,
    clear: bool,
    unset: Vec<String>,
    own: Vec<(String, String)>,
}

impl Layer {
    /// The layers the words before a program describe: the assignments before the first
    /// `env` are the inherited environment, and every `env` invocation after that is a
    /// layer of its own, so `env A=1 env -u A cmd` removes `A` in the inner layer after
    /// the outer one set it.
    fn stack(prefix: &[&str]) -> Vec<Layer> {
        let assignments = |ws: &[&str]| -> Vec<(String, String)> {
            ws.iter()
                .filter(|w| is_assignment(w))
                .filter_map(|w| w.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let env_at: Vec<usize> = prefix
            .iter()
            .enumerate()
            .filter(|(_, w)| !is_assignment(w) && basename(w) == "env")
            .map(|(i, _)| i)
            .collect();
        let first = env_at.first().copied().unwrap_or(prefix.len());
        let mut layers = vec![Layer {
            pre: assignments(&prefix[..first]),
            ..Layer::default()
        }];
        for (n, &at) in env_at.iter().enumerate() {
            let until = env_at.get(n + 1).copied().unwrap_or(prefix.len());
            let words = &prefix[at + 1..until];
            let mut layer = Layer {
                own: assignments(words),
                ..Layer::default()
            };
            let mut it = words.iter().filter(|w| !is_assignment(w));
            while let Some(w) = it.next() {
                match *w {
                    "-i" | "--ignore-environment" => layer.clear = true,
                    "-u" | "--unset" => layer.unset.extend(it.next().map(|n| n.to_string())),
                    _ if w.starts_with("--unset=") => {
                        layer.unset.push(w["--unset=".len()..].to_string())
                    }
                    _ if w.starts_with("-u") && w.len() > 2 => layer.unset.push(w[2..].to_string()),
                    _ => {}
                }
            }
            layers.push(layer);
        }
        layers
    }

    /// A variable as the program sees it through this stack of layers, innermost last:
    /// an `env`-line assignment wins; a removal or a cleared environment ends the search;
    /// an inherited assignment is next; the outer layers follow, then `setup.env`, whose
    /// secret-sourced values the gate cannot read.
    fn resolve<'a>(layers: &'a [Layer], name: &str, setup: &'a Setup) -> Resolved<'a> {
        let last = |list: &'a [(String, String)]| {
            list.iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        for layer in layers.iter().rev() {
            if let Some(v) = last(&layer.own) {
                return Resolved::Plain(v);
            }
            if layer.clear || layer.unset.iter().any(|u| u == name) {
                return Resolved::Absent;
            }
            if let Some(v) = last(&layer.pre) {
                return Resolved::Plain(v);
            }
        }
        match setup.env.get(name) {
            Some(EnvValue::Plain(v)) => Resolved::Plain(v.as_str()),
            Some(EnvValue::Secret(_)) => Resolved::Secret,
            None => Resolved::Absent,
        }
    }
}

/// What a variable resolves to for the program.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Resolved<'a> {
    Plain(&'a str),
    /// Named in `setup.env` with a `{from: …}` source: set at run time, unreadable here.
    Secret,
    Absent,
}

/// The variable a value made of one bare reference names (`$NAME`, `${NAME}`).
fn reference(v: &str) -> Option<&str> {
    let name = v
        .strip_prefix("${")
        .and_then(|r| r.strip_suffix('}'))
        .or_else(|| v.strip_prefix('$'))?;
    let valid = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then_some(name)
}

/// `NAME=value` with a name the shell accepts.
fn is_assignment(w: &str) -> bool {
    w.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !name.starts_with(|c: char| c.is_ascii_digit())
    })
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// Splits a command line the way a POSIX shell does for a simple command: whitespace
/// separates words, single quotes keep everything, double quotes keep everything but
/// allow backslash escapes, a backslash escapes the next character, a backslash before
/// a newline joins the lines, and an unquoted `#` at a word boundary starts a comment.
/// Unquoted control operators and parentheses are words of their own, while a `$(…)`
/// substitution stays inside its word; redirections and their targets are dropped. No expansion, no execution. An unterminated quote or a
/// redirection without a target returns what was read so far and the error.
pub(crate) fn shell_words(line: &str) -> (Vec<Token>, Option<String>) {
    /// Reads a parenthesised body verbatim into `word` up to and including the `)` that
    /// closes it, honouring nested parentheses, quotes and backslash escapes, as in
    /// `$(…)`, `>(…)` and `<(…)`. Returns whether the body was closed.
    fn read_balanced(
        chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
        word: &mut String,
    ) -> bool {
        let mut depth = 1usize;
        // A `#` that begins a word inside the substitution starts a comment to the end of
        // its line; quotes and parentheses inside it mean nothing.
        let mut at_word_start = true;
        while let Some(q) = chars.next() {
            word.push(q);
            let starts_word = at_word_start;
            at_word_start = q.is_whitespace() || matches!(q, ';' | '&' | '|' | '(');
            if q == '#' && starts_word {
                for n in chars.by_ref() {
                    word.push(n);
                    if n == '\n' {
                        break;
                    }
                }
                at_word_start = true;
                continue;
            }
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
                        } else if n == '$' && chars.peek() == Some(&'(') {
                            // A substitution inside the quotes has quotes of its own.
                            word.extend(chars.next());
                            if !read_balanced(chars, word) {
                                return false;
                            }
                        } else if n == '"' {
                            break;
                        }
                    }
                }
                '`' => {
                    if !read_backticks(chars, word) {
                        return false;
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
    /// Reads a backtick substitution verbatim into `word` up to and including the closing
    /// backtick, honouring backslash escapes. Returns whether it was closed.
    fn read_backticks(
        chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
        word: &mut String,
    ) -> bool {
        while let Some(q) = chars.next() {
            word.push(q);
            match q {
                '\\' => word.extend(chars.next()),
                '`' => return true,
                _ => {}
            }
        }
        false
    }
    let mut words: Vec<Token> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    let mut error = None;
    // The word after a redirection operator is its target, not an argument.
    let mut drop_next_word = false;
    // Whether any part of the current word was quoted or escaped: `"2">f` is an argument
    // `2` and a redirection, not descriptor 2 redirected.
    let mut word_quoted = false;
    let ends_command = |words: &[Token]| matches!(words.last(), Some(Token::Op(o)) if o == ";");
    let finish = |words: &mut Vec<Token>, word: &mut String, drop: &mut bool, quoted: &mut bool| {
        *quoted = false;
        if *drop {
            word.clear();
            *drop = false;
        } else {
            words.push(Token::Word(std::mem::take(word)));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    finish(&mut words, &mut word, &mut drop_next_word, &mut word_quoted);
                    in_word = false;
                }
                // An unescaped newline ends the command like `;` does, and a redirection
                // still waiting for its target is the shell's error.
                if c == '\n' {
                    if drop_next_word {
                        error.get_or_insert_with(|| "redirection without a target".to_string());
                        drop_next_word = false;
                    }
                    if !words.is_empty() && !ends_command(&words) {
                        words.push(Token::Op(";".to_string()));
                    }
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
                let mut more = 0usize;
                while let Some(&next) = chars.peek()
                    && matches!(next, '>' | '<')
                {
                    chars.next();
                    more += usize::from(next == c);
                }
                // A here-document's body (`<<`, `<<-`) would read as commands; the gate
                // refuses the line rather than guess where the body ends. A here-string
                // (`<<<`) has no body and is an ordinary redirection.
                if c == '<' && more == 1 {
                    error.get_or_insert_with(|| {
                        "here-documents are not supported in setup.command".to_string()
                    });
                }
                // `>|` overrides `noclobber`; the `|` belongs to the redirection.
                if chars.peek() == Some(&'|') {
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
                words.push(Token::Op(op));
            }
            // An unquoted `#` at a word boundary comments out the rest of its line only;
            // the line's end still ends the command.
            '#' if !in_word => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        if drop_next_word {
                            error.get_or_insert_with(|| "redirection without a target".to_string());
                            drop_next_word = false;
                        }
                        if !words.is_empty() && !ends_command(&words) {
                            words.push(Token::Op(";".to_string()));
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
                        // A backtick substitution inside the quotes is kept verbatim too.
                        '`' => {
                            word.push('`');
                            if !read_backticks(&mut chars, &mut word) {
                                error.get_or_insert_with(|| {
                                    "unterminated backtick substitution".to_string()
                                });
                            }
                        }
                        // A substitution keeps its own quotes; it is kept verbatim.
                        '$' if chars.peek() == Some(&'(') => {
                            word.push('$');
                            word.extend(chars.next());
                            if !read_balanced(&mut chars, &mut word) {
                                error.get_or_insert_with(|| {
                                    "unterminated command substitution".to_string()
                                });
                            }
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
            // A backtick substitution is part of its word, kept verbatim like `$(…)`; a `;`
            // or `#` inside it is not a boundary or a comment.
            '`' => {
                in_word = true;
                word_quoted = true;
                word.push('`');
                if !read_backticks(&mut chars, &mut word) {
                    error.get_or_insert_with(|| "unterminated backtick substitution".to_string());
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
        let (tokens, parse_error) = shell_words(&setup.command);
        // The switches belong to the simple command that runs setup.sh. The line may `cd`
        // first, or reach the script through `env`, `sudo`, `nohup` or an interpreter with
        // options of their own, so the script is found by name within its command, and
        // every `NAME=value` word before it is an assignment it will see. A command runs
        // the script when only wrappers and their arguments stand before it; `test -x
        // ./setup.sh`, `rm ./setup.sh` or `env test -x ./setup.sh` merely name it. When no
        // command on the line qualifies (an unlisted wrapper), the first one naming it is
        // taken, so a line that names setup.sh is never silently ignored.
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
        // A command's prefix: the layers an embedded command inherits, and its words.
        fn prefix_of(tokens: &[Token]) -> (Vec<Layer>, Vec<&str>) {
            let mut layers = Vec::new();
            let mut words = Vec::new();
            for t in tokens {
                match t {
                    Token::Inherits(l) => layers.clone_from(l),
                    Token::Word(w) => words.push(w.as_str()),
                    Token::Op(_) => {}
                }
            }
            (layers, words)
        }
        fn non_assignments<'a>(words: &[&'a str]) -> Vec<&'a str> {
            words
                .iter()
                .copied()
                .filter(|w| !is_assignment(w))
                .collect()
        }
        // `bash -c '…'` in command position runs the string as a command line of its own:
        // its words replace the interpreter's, every command inside inherits the
        // environment the words around the interpreter describe, resolved at that
        // boundary, and anything after the string is `$0` and positional parameters, not
        // switches. A `bash` that is another command's argument is left alone.
        // What an interpreter in command position does with what follows it.
        enum Interpreter<'a> {
            /// `-c`: runs the string, the first word after the option clusters (`-c`,
            /// `-lc`, `-l -c`, `-xe -c`); the count is the words up to and including it.
            Runs(usize, &'a str),
            /// `-n` in a cluster: reads without running, so the command does nothing.
            NoExec,
            /// Anything else, including an option with an operand (`-o pipefail`): the
            /// words are left alone.
            Other,
        }
        // Option clusters are read in order: a `-` cluster sets its letters, a `+` cluster
        // clears them (`-n +n` runs after all), and the outcome is decided at the first
        // word that is not a cluster, or at the end of the command.
        fn interpreter_does(words: &[Token]) -> Interpreter<'_> {
            let mut has_c = false;
            let mut no_exec = false;
            let mut n = 0;
            while let Some(Token::Word(w)) = words.get(n) {
                let cluster = w.len() > 1
                    && w.starts_with(['-', '+'])
                    && w[1..].chars().all(|c| c.is_ascii_alphabetic());
                if !cluster {
                    break;
                }
                let sets = w.starts_with('-');
                if w.contains('n') {
                    no_exec = sets;
                }
                if w.contains('c') {
                    has_c = sets;
                }
                n += 1;
            }
            // `--` ends the options; the string follows it.
            if matches!(words.get(n), Some(Token::Word(w)) if w == "--") {
                n += 1;
            }
            match words.get(n) {
                _ if no_exec => Interpreter::NoExec,
                Some(Token::Word(w)) if has_c => Interpreter::Runs(n + 1, w.as_str()),
                _ => Interpreter::Other,
            }
        }
        let mut parse_error = parse_error;
        let mut expanded: Vec<Token> = Vec::with_capacity(tokens.len());
        let mut i = 0;
        while i < tokens.len() {
            let start = expanded.iter().rposition(Token::is_op).map_or(0, |p| p + 1);
            let (outer_layers, prefix) = prefix_of(&expanded[start..]);
            let interpreter = matches!(&tokens[i], Token::Word(w)
                if matches!(basename(w), "bash" | "sh" | "dash" | "zsh"));
            let does = if interpreter && is_invocation(&non_assignments(&prefix)) {
                interpreter_does(&tokens[i + 1..])
            } else {
                Interpreter::Other
            };
            match does {
                Interpreter::Runs(skip, script) => {
                    let (inner, err) = shell_words(script);
                    if parse_error.is_none() {
                        parse_error = err;
                    }
                    let mut layers = outer_layers;
                    layers.extend(Layer::stack(&prefix));
                    expanded.truncate(start);
                    let mut at_segment_start = true;
                    for t in inner {
                        if t.is_op() {
                            at_segment_start = true;
                        } else if at_segment_start {
                            expanded.push(Token::Inherits(layers.clone()));
                            at_segment_start = false;
                        }
                        expanded.push(t);
                    }
                    i += 1 + skip;
                    while i < tokens.len() && !tokens[i].is_op() {
                        i += 1;
                    }
                }
                // A command that runs nothing cannot name the script that runs: it is
                // dropped, prefix included, so a line with only `bash -n ./setup.sh` is
                // one that does not run setup.sh.
                Interpreter::NoExec => {
                    expanded.truncate(start);
                    while i < tokens.len() && !tokens[i].is_op() {
                        i += 1;
                    }
                }
                Interpreter::Other => {
                    expanded.push(tokens[i].clone());
                    i += 1;
                }
            }
        }
        /// A simple command on the line that names setup.sh.
        struct Naming {
            layers: Vec<Layer>,
            /// How many of `layers` the command inherits from the shells around it; the
            /// rest are its own line. The shell expands the command's arguments in the
            /// inherited environment.
            inherited: usize,
            args: Vec<String>,
            invokes: bool,
        }
        let naming: Vec<Naming> = expanded
            .split(Token::is_op)
            .filter_map(|seg| {
                let (mut layers, words) = prefix_of(seg);
                let at = words
                    .iter()
                    .position(|w| !is_assignment(w) && basename(w) == "setup.sh")?;
                let before = non_assignments(&words[..at]);
                let inherited = layers.len();
                layers.extend(Layer::stack(&words[..at]));
                Some(Naming {
                    layers,
                    inherited,
                    args: words[at + 1..].iter().map(|w| w.to_string()).collect(),
                    invokes: is_invocation(&before),
                })
            })
            .collect();
        let segment = naming
            .iter()
            .position(|n| n.invokes)
            .or_else(|| (!naming.is_empty()).then_some(0))
            .map(|i| &naming[i]);
        let runs_setup_sh = segment.is_some();
        // A variable as setup.sh will see it, through every layer of assignments and `env`
        // controls around it, then `setup.env`.
        let source = |name: &str| {
            segment.map_or(Resolved::Absent, |n| Layer::resolve(&n.layers, name, setup))
        };
        // The shell expands the words after setup.sh before setup.sh reads them, in the
        // shell's own environment: what the shells around the command hand it (an outer
        // `env` around a `bash -c`), then `setup.env`. A bare reference to a plain value
        // becomes the words an unquoted expansion yields, none for an empty value, so an
        // option takes the operand setup.sh will see. Any other expression stays a word of
        // its own: as an operand it is the value as written; in switch position it is
        // reported below as not readable, not refused.
        let shell_env = |name: &str| {
            segment.map_or(Resolved::Absent, |n| {
                Layer::resolve(&n.layers[..n.inherited], name, setup)
            })
        };
        let mut words: Vec<String> = Vec::new();
        for w in segment.map(|n| n.args.as_slice()).unwrap_or_default() {
            match reference(w).map(shell_env) {
                Some(Resolved::Plain(v)) if !v.contains(['$', '`']) => {
                    words.extend(v.split_whitespace().map(String::from));
                }
                _ => words.push(w.clone()),
            }
        }
        let plain = |name: &str| match source(name) {
            Resolved::Plain(v) => Some(v),
            _ => None,
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
            parse_error,
            env_error: None,
            switch_error: None,
            unexpanded: Vec::new(),
            runs_setup_sh,
        };
        let mut i = 0;
        while i < words.len() {
            let w = words[i].clone();
            let w = w.as_str();
            let value = words.get(i + 1).cloned();
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
                "-y"
                | "--ignore-preflight-errors"
                | "--skip-core"
                | "--skip-rest"
                | "--install-dpf"
                | "--skip-dpf"
                | "--install-rms"
                | "--skip-rms"
                | "--install-vault"
                | "--skip-vault"
                | "--with-observability"
                | "--install-contour"
                | "--debug" => c.switches.push(w.to_string()),
                // An expression the shell would expand that the expansion above could not
                // read: what it adds is unknown, so it is reported, not refused.
                _ if w.contains(['$', '`']) => c.unexpanded.push(w.to_string()),
                // setup.sh's usage arm exits 1 on any other word, a misspelled switch
                // included.
                _ => {
                    c.switch_error
                        .get_or_insert_with(|| format!("setup.sh does not accept `{w}`"));
                }
            }
            i += 1;
        }
        // setup.sh starts from NICO_INSTALL_*, lets NICO_SKIP_*=true and then the switches
        // override it, and exits unless the result is exactly `true` or `false`. Checked
        // against the switches as parsed, since a word an option consumed as its value
        // (`--core-values --skip-rms`) decides nothing.
        let invalid_install = |install: &str, skip: &str, skip_sw: &str, install_sw: &str| {
            let overridden = plain(skip) == Some("true") || c.has(skip_sw) || c.has(install_sw);
            match plain(install) {
                // `${NICO_INSTALL_DPF:-true}`: setup.sh takes an empty value as unset.
                Some(v) if !v.is_empty() && !overridden && v != "true" && v != "false" => {
                    Some(format!("{install} must be true or false (got '{v}')"))
                }
                _ => None,
            }
        };
        if runs_setup_sh {
            // Every component control setup.sh reads: the install variable it validates,
            // the skip variable and the two switches that override both.
            const CONTROLS: [(&str, &str, &str, &str); 3] = [
                (
                    "NICO_INSTALL_DPF",
                    "NICO_SKIP_DPF",
                    "--skip-dpf",
                    "--install-dpf",
                ),
                (
                    "NICO_INSTALL_RMS",
                    "NICO_SKIP_RMS",
                    "--skip-rms",
                    "--install-rms",
                ),
                (
                    "NICO_INSTALL_VAULT",
                    "NICO_SKIP_VAULT",
                    "--skip-vault",
                    "--install-vault",
                ),
            ];
            const FLAGS: [(&str, &str); 2] = [
                ("WITH_OBSERVABILITY", "--with-observability"),
                ("NICO_INSTALL_CONTOUR", "--install-contour"),
            ];
            // A secret-sourced control is unreadable here, unless a switch, or the other
            // control of the pair with a plain value (`NICO_SKIP_*=true` for the install
            // variable, `NICO_INSTALL_*=false` for the skip variable), settles the
            // component whatever the secret holds.
            let secret = |name: &str, switches: &[&str], settles: Option<(&str, &str)>| {
                let decided = switches.iter().any(|s| c.has(s))
                    || settles.is_some_and(|(var, value)| plain(var) == Some(value));
                (matches!(source(name), Resolved::Secret) && !decided).then(|| {
                    format!(
                        "{name} comes from a secret source the gate cannot read; set a plain \
                         value or decide it with {}",
                        switches.join("/")
                    )
                })
            };
            c.env_error = CONTROLS
                .iter()
                .find_map(|(install, skip, skip_sw, install_sw)| {
                    secret(install, &[skip_sw, install_sw], Some((skip, "true")))
                        .or_else(|| secret(skip, &[skip_sw, install_sw], Some((install, "false"))))
                        .or_else(|| invalid_install(install, skip, skip_sw, install_sw))
                })
                .or_else(|| {
                    FLAGS
                        .iter()
                        .find_map(|(name, sw)| secret(name, &[sw], None))
                });
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
        assert!(c.parse_error.is_none());
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

    /// `-c` inside an option cluster, or after other options, runs the string too.
    #[test]
    fn dash_c_in_an_option_cluster_runs_the_string() {
        for line in [
            "bash -lc './setup.sh --skip-dpf --skip-rms'",
            "bash -l -c './setup.sh --skip-dpf --skip-rms'",
            "bash -xe -c './setup.sh --skip-dpf --skip-rms' bash --install-rms",
        ] {
            let c = cmd(line);
            assert!(
                !c.installs_rms() && !c.installs_dpf() && c.parse_error.is_none(),
                "{line}"
            );
        }
        let c = cmd("bash -nc './setup.sh --skip-dpf'; ./setup.sh --skip-rms");
        assert!(c.installs_dpf() && !c.installs_rms(), "`-n` runs nothing");
        let c = cmd("bash -c -n ./setup.sh --skip-rms; ./setup.sh --skip-dpf");
        assert!(
            !c.installs_dpf() && c.installs_rms(),
            "unquoted `-n` command"
        );
        let c = cmd("cd helm-prereqs && bash -n ./setup.sh --skip-rms");
        assert!(!c.runs_setup_sh(), "`-n` alone runs nothing");
        let c = cmd("bash -n +n ./setup.sh --skip-rms");
        assert!(
            c.runs_setup_sh() && !c.installs_rms(),
            "`+n` turns noexec off"
        );
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
        let c = cmd("echo '(' ./setup.sh --skip-rms && ./setup.sh --skip-dpf");
        assert!(
            c.installs_rms() && !c.installs_dpf(),
            "a quoted parenthesis is a word, not a boundary"
        );
        let c = cmd("./setup.sh --core-values ')' --skip-rms");
        assert_eq!(
            c.core_values.as_deref(),
            Some(")"),
            "a quoted parenthesis is an argument"
        );
        assert!(!c.installs_rms());
        let c = cmd("echo ';' ./setup.sh --skip-rms; ./setup.sh --skip-dpf");
        assert!(
            c.installs_rms() && !c.installs_dpf(),
            "a quoted semicolon likewise"
        );
        let c = cmd("env NICO_SKIP_RMS=true bash -c 'env -u NICO_SKIP_RMS ./setup.sh --skip-dpf'");
        assert!(
            c.installs_rms() && !c.installs_dpf(),
            "the outer `env` line is resolved before the inner `env -u`"
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
        assert!(!c.installs_rms() && c.parse_error.is_none());
        let c = cmd(r#"./setup.sh --core-values "$(printf '%s' "site;values.yaml")" --skip-rms"#);
        assert_eq!(
            c.core_values.as_deref(),
            Some(r#"$(printf '%s' "site;values.yaml")"#)
        );
        assert!(
            !c.installs_rms(),
            "a substitution inside quotes keeps its own quotes"
        );
        let c = cmd("./setup.sh --skip-rms <<< 'yes'");
        assert!(
            !c.installs_rms() && c.parse_error.is_none(),
            "a here-string is a plain redirection"
        );
        let c = cmd(
            "./setup.sh --core-values \"$(printf values.yaml # user's directory\n)\" --skip-rms",
        );
        assert!(
            c.core_values.is_some() && !c.installs_rms() && c.parse_error.is_none(),
            "a comment inside a substitution is skipped: {:?}",
            c.parse_error
        );
        let c = cmd("cat <<EOF\n./setup.sh --skip-dpf\nEOF\n./setup.sh --skip-rms");
        assert!(
            c.parse_error
                .as_deref()
                .is_some_and(|e| e.contains("here-document")),
            "a here-document is refused rather than read as commands: {:?}",
            c.parse_error
        );
        let c = cmd("./setup.sh --skip-rms >|setup.log");
        assert!(
            !c.installs_rms() && c.parse_error.is_none(),
            "`>|` is one redirection"
        );
        let c = cmd(r#"TAG=$(echo "$(echo "(")") ./setup.sh --skip-rms"#);
        assert!(
            !c.installs_rms() && c.parse_error.is_none(),
            "nested substitutions in quotes"
        );
        let c = cmd("./setup.sh > >(tee setup.log) --skip-rms");
        assert!(
            !c.installs_rms() && c.parse_error.is_none(),
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
        for line in [
            "./setup.sh --skip-rms >\necho x",
            "./setup.sh > # comment\necho x",
        ] {
            let (tokens, err) = shell_words(line);
            assert!(
                err.is_some(),
                "a newline does not supply a target: {line:?}"
            );
            assert!(
                tokens
                    .iter()
                    .any(|t| matches!(t, Token::Word(w) if w == "echo")),
                "the next line's command is not the target: {tokens:?}"
            );
        }
        let (tokens, err) = shell_words("./setup.sh > ; echo x");
        assert!(
            err.is_some(),
            "a dangling redirection before an operator is an error too"
        );
        let words: Vec<&str> = tokens.iter().filter_map(Token::text).collect();
        assert_eq!(
            words,
            ["./setup.sh", ";", "echo", "x"],
            "an operator ends a dangling redirection"
        );
    }

    fn with(command: &str, env: &[(&str, EnvValue)]) -> SetupCommand {
        SetupCommand::parse(&Setup {
            command: command.to_string(),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        })
    }

    fn secret() -> EnvValue {
        EnvValue::Secret(crate::sitedef::SecretSource {
            from: "env".to_string(),
            path: None,
            name: Some("SITE_SKIP_RMS".to_string()),
        })
    }

    #[test]
    fn each_env_wrapper_is_its_own_layer() {
        let c = cmd("env NICO_SKIP_RMS=true env -u NICO_SKIP_RMS ./setup.sh");
        assert!(
            c.installs_rms(),
            "the inner removal wins over the outer assignment"
        );
    }

    #[test]
    fn a_secret_sourced_control_needs_a_plain_value_or_a_decision() {
        let c = with("./setup.sh", &[("NICO_SKIP_RMS", secret())]);
        assert!(
            c.env_error
                .as_deref()
                .is_some_and(|e| e.contains("NICO_SKIP_RMS") && e.contains("secret")),
            "{:?}",
            c.env_error
        );
        let c = with("./setup.sh --skip-rms", &[("NICO_SKIP_RMS", secret())]);
        assert!(
            c.env_error.is_none() && !c.installs_rms(),
            "a switch decides over the secret"
        );
        let c = with(
            "./setup.sh --core-values --skip-rms",
            &[("NICO_SKIP_RMS", secret())],
        );
        assert!(
            c.env_error.is_some(),
            "a word consumed as an option's value decides nothing"
        );
        let c = with(
            "./setup.sh",
            &[
                ("NICO_INSTALL_DPF", secret()),
                ("NICO_SKIP_DPF", EnvValue::Plain("true".into())),
            ],
        );
        assert!(
            c.env_error.is_none() && !c.installs_dpf(),
            "a plain skip decides before setup.sh reads the install variable"
        );
        let c = with("./setup.sh", &[("WITH_OBSERVABILITY", secret())]);
        assert!(
            c.env_error.is_some(),
            "the observability flag is a control too"
        );
    }

    /// `./setup.sh $FLAGS`: the shell expands the reference before setup.sh reads it.
    #[test]
    fn a_reference_after_setup_sh_is_expanded_or_reported() {
        let c = with(
            "./setup.sh $FLAGS",
            &[("FLAGS", EnvValue::Plain("--skip-rms --skip-dpf".into()))],
        );
        assert!(
            !c.installs_rms() && !c.installs_dpf() && c.switch_error.is_none(),
            "a plain value is split into the switches it holds"
        );
        let c = with(
            "env FLAGS=--skip-dpf ./setup.sh $FLAGS --skip-rms",
            &[("FLAGS", EnvValue::Plain("--install-dpf".into()))],
        );
        assert!(
            c.installs_dpf() && !c.installs_rms(),
            "the shell expands the argument before `env` sets FLAGS for setup.sh"
        );
        let c = with(
            "env FLAGS=--install-dpf bash -c './setup.sh $FLAGS --skip-rms'",
            &[("FLAGS", EnvValue::Plain("--skip-dpf".into()))],
        );
        assert!(
            c.installs_dpf() && !c.installs_rms(),
            "the inner shell expands the argument in the environment it inherited"
        );
        let c = with(
            "./setup.sh --core-values $EMPTY values/nico-core.yaml --skip-rms",
            &[("EMPTY", EnvValue::Plain(String::new()))],
        );
        assert_eq!(
            c.core_values.as_deref(),
            Some("values/nico-core.yaml"),
            "an empty expansion vanishes before the option takes its operand"
        );
        assert!(!c.installs_rms() && c.switch_error.is_none());
        let c = with("./setup.sh $FLAGS --skip-rms", &[("FLAGS", secret())]);
        assert!(
            c.switch_error.is_none() && !c.installs_rms(),
            "an unreadable value is not an unknown switch"
        );
        assert_eq!(c.unexpanded, vec!["$FLAGS".to_string()]);
        let c = cmd("./setup.sh $(cat flags)");
        assert_eq!(c.unexpanded, vec!["$(cat flags)".to_string()]);
    }

    #[test]
    fn a_plain_install_false_settles_a_secret_skip() {
        let c = with(
            "./setup.sh",
            &[
                ("NICO_SKIP_DPF", secret()),
                ("NICO_INSTALL_DPF", EnvValue::Plain("false".into())),
            ],
        );
        assert!(
            c.env_error.is_none() && !c.installs_dpf(),
            "the component is skipped whatever the secret holds"
        );
    }

    #[test]
    fn a_double_dash_ends_the_interpreter_options_before_the_string() {
        let c = cmd("bash -c -- './setup.sh --skip-rms'");
        assert!(c.runs_setup_sh() && !c.installs_rms());
    }

    #[test]
    fn a_read_write_redirection_is_not_a_here_document() {
        let c = cmd("./setup.sh --skip-rms <>/dev/null");
        assert!(!c.installs_rms() && c.parse_error.is_none());
    }

    #[test]
    fn the_vault_install_variable_is_validated_like_the_others() {
        let c = with(
            "./setup.sh",
            &[("NICO_INSTALL_VAULT", EnvValue::Plain("yes".into()))],
        );
        assert_eq!(
            c.env_error.as_deref(),
            Some("NICO_INSTALL_VAULT must be true or false (got 'yes')")
        );
    }

    #[test]
    fn a_word_setup_sh_does_not_accept_is_refused() {
        let c = cmd("./setup.sh --skip-rsm --skip-dpf");
        assert_eq!(
            c.switch_error.as_deref(),
            Some("setup.sh does not accept `--skip-rsm`")
        );
        assert!(!c.installs_dpf(), "the known switch still counts");
        let c = cmd("./setup.sh --core-values values/a.yaml extra");
        assert!(
            c.switch_error.is_some(),
            "a stray word is refused like setup.sh does"
        );
    }

    #[test]
    fn backticks_are_one_word_and_an_unterminated_one_is_an_error() {
        let c = cmd("./setup.sh --core-values `echo v;x` --skip-rms");
        assert_eq!(
            c.core_values.as_deref(),
            Some("`echo v;x`"),
            "a `;` inside backticks does not split the command"
        );
        assert!(!c.installs_rms() && c.parse_error.is_none());
        let c = cmd("TAG=`echo '#'` ./setup.sh --skip-rms");
        assert!(
            !c.installs_rms() && c.parse_error.is_none(),
            "a `#` inside backticks is text"
        );
        let c = cmd("./setup.sh --core-values \"`echo a`.yaml\" --skip-rms");
        assert_eq!(c.core_values.as_deref(), Some("`echo a`.yaml"));
        let c = cmd("./setup.sh `echo --skip-rms");
        assert!(
            c.parse_error
                .as_deref()
                .is_some_and(|e| e.contains("backtick"))
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
        assert_eq!(c.parse_error.as_deref(), Some("unterminated double quote"));
    }
}
