//! Reading OpenSSH's `~/.ssh/config`.
//!
//! Supporting this file is what lets BestTerm inherit a setup someone already has — the jump hosts,
//! the per-host identities, the aliases their team documented years ago — instead of asking them to
//! enter it all again.
//!
//! # The rule everyone gets wrong
//!
//! **The first value wins, not the last.** The manual is explicit: "Unless noted otherwise, for each
//! configuration directive, the first specified value will be used." A parser that lets later blocks
//! overwrite earlier ones will appear to work on most files and then quietly connect to the wrong
//! host on the one file that is organised the way the manual recommends — specific hosts first,
//! `Host *` last.
//!
//! `IdentityFile` is the documented exception: those accumulate.
//!
//! # What is deliberately not supported
//!
//! `Match exec` runs a shell command to decide whether a block applies. Running commands out of a
//! configuration file while merely listing someone's hosts is not a default worth having, so such a
//! block never matches and the caller is told through [`Query::unsupported`].

use std::collections::BTreeMap;

/// A parsed configuration.
#[derive(Clone, Debug, Default)]
pub struct SshConfig {
    blocks: Vec<Block>,
}

#[derive(Clone, Debug)]
struct Block {
    condition: Condition,
    options: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
enum Condition {
    /// Options before any `Host` or `Match`: they apply to everything.
    Global,
    /// `Host pattern [pattern ...]`.
    Host(Vec<HostPattern>),
    /// `Match criteria`.
    Match(Vec<Criterion>),
}

#[derive(Clone, Debug)]
struct HostPattern {
    pattern: String,
    negated: bool,
}

#[derive(Clone, Debug)]
enum Criterion {
    /// `all`.
    All,
    /// `host <patterns>`.
    Host(Vec<HostPattern>),
    /// `originalhost <patterns>`, before any canonicalisation. We never canonicalise, so it is the
    /// same string as `host`; kept separate because the file may say either.
    OriginalHost(Vec<HostPattern>),
    /// `user <patterns>` — the remote user.
    User(Vec<HostPattern>),
    /// `localuser <patterns>`.
    LocalUser(Vec<HostPattern>),
    /// A criterion this parser will not evaluate, such as `exec`.
    Unsupported(String),
}

/// What is known about the connection when the configuration is queried.
#[derive(Clone, Debug, Default)]
pub struct QueryContext<'a> {
    /// The name the user asked for.
    pub host: &'a str,
    /// The remote user, if one was chosen before consulting the file.
    pub user: Option<&'a str>,
    /// The local account name.
    pub local_user: Option<&'a str>,
}

/// The settings that apply to one host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// Every keyword that applied, lowercased, with the winning value.
    values: BTreeMap<String, String>,
    /// `IdentityFile` entries, in the order they were found.
    identity_files: Vec<String>,
    /// Criteria that were skipped rather than evaluated, e.g. `exec`.
    pub unsupported: Vec<String>,
}

impl Query {
    /// The winning value for a keyword, which may be spelled in any case.
    pub fn get(&self, keyword: &str) -> Option<&str> {
        self.values
            .get(&keyword.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// The address to actually connect to: `HostName` if set, otherwise the name asked for.
    pub fn host_name<'a>(&'a self, requested: &'a str) -> &'a str {
        self.get("hostname").unwrap_or(requested)
    }

    /// `Port`, defaulting to 22.
    pub fn port(&self) -> u16 {
        self.get("port")
            .and_then(|value| value.parse().ok())
            .filter(|port| *port != 0)
            .unwrap_or(22)
    }

    /// `User`.
    pub fn user(&self) -> Option<&str> {
        self.get("user")
    }

    /// `IdentityFile` entries, in order. Unlike other keywords these accumulate.
    pub fn identity_files(&self) -> &[String] {
        &self.identity_files
    }

    /// `ProxyJump`, split into hops, nearest first.
    ///
    /// `none` means "no jump" and yields an empty list — it is how a later, more specific block
    /// cancels a `ProxyJump` inherited from a broader one.
    pub fn proxy_jump(&self) -> Vec<JumpHop> {
        let Some(raw) = self.get("proxyjump") else {
            return Vec::new();
        };
        if raw.eq_ignore_ascii_case("none") {
            return Vec::new();
        }
        raw.split(',')
            .map(str::trim)
            .filter(|hop| !hop.is_empty())
            .map(JumpHop::parse)
            .collect()
    }

    /// Whether a yes/no keyword is on.
    pub fn flag(&self, keyword: &str) -> Option<bool> {
        match self.get(keyword)?.to_ascii_lowercase().as_str() {
            "yes" | "true" => Some(true),
            "no" | "false" => Some(false),
            _ => None,
        }
    }
}

/// One hop of a `ProxyJump` chain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JumpHop {
    /// Host to connect through.
    pub host: String,
    /// User for that hop, when the chain names one.
    pub user: Option<String>,
    /// Port for that hop, when the chain names one.
    pub port: Option<u16>,
}

impl JumpHop {
    /// Parse a `[user@]host[:port]` hop.
    fn parse(raw: &str) -> Self {
        let (user, rest) = match raw.rsplit_once('@') {
            Some((user, rest)) => (Some(user.to_string()), rest),
            None => (None, raw),
        };

        // Bracketed IPv6, `[::1]:2222`.
        if let Some(inner) = rest.strip_prefix('[')
            && let Some((address, tail)) = inner.split_once(']')
        {
            let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
            return Self {
                host: address.to_string(),
                user,
                port,
            };
        }

        // A bare IPv6 address contains colons that are not a port separator.
        let (host, port) = match rest.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host, port.parse().ok()),
            _ => (rest, None),
        };

        Self {
            host: host.to_string(),
            user,
            port,
        }
    }
}

impl SshConfig {
    /// Parse a configuration that contains no `Include`.
    pub fn parse(text: &str) -> Self {
        Self::parse_with_includes(text, &mut |_| Vec::new())
    }

    /// Parse a configuration, resolving `Include` through `resolve`.
    ///
    /// `resolve` receives the raw pattern and returns the contents of the matching files in lexical
    /// order — the order the manual specifies. Passing the resolution in rather than reading files
    /// here keeps the parser testable without a filesystem, and keeps the decision about which
    /// directory a relative include is relative to with the caller that knows.
    pub fn parse_with_includes(text: &str, resolve: &mut dyn FnMut(&str) -> Vec<String>) -> Self {
        let mut blocks = Vec::new();
        parse_into(text, &mut blocks, resolve, 0);
        Self { blocks }
    }

    /// Resolve the settings for one host.
    pub fn query(&self, context: &QueryContext<'_>) -> Query {
        let mut result = Query::default();

        for block in &self.blocks {
            let applies = match &block.condition {
                Condition::Global => true,
                Condition::Host(patterns) => patterns_match(patterns, context.host),
                Condition::Match(criteria) => {
                    match_applies(criteria, context, &mut result.unsupported)
                }
            };
            if !applies {
                continue;
            }

            for (keyword, value) in &block.options {
                if keyword == "identityfile" {
                    // The documented exception: these accumulate rather than compete.
                    result.identity_files.push(value.clone());
                    continue;
                }
                // First wins. `entry` inserts only when vacant, which is the whole rule.
                result
                    .values
                    .entry(keyword.clone())
                    .or_insert_with(|| value.clone());
            }
        }

        result.unsupported.sort_unstable();
        result.unsupported.dedup();
        result
    }
}

/// How deep `Include` may nest before we assume a cycle.
const MAX_INCLUDE_DEPTH: usize = 16;

fn parse_into(
    text: &str,
    blocks: &mut Vec<Block>,
    resolve: &mut dyn FnMut(&str) -> Vec<String>,
    depth: usize,
) {
    let mut current = Block {
        condition: Condition::Global,
        options: Vec::new(),
    };

    for raw in text.lines() {
        let Some((keyword, value)) = split_line(raw) else {
            continue;
        };

        match keyword.as_str() {
            "host" => {
                blocks.push(std::mem::replace(
                    &mut current,
                    Block {
                        condition: Condition::Host(parse_host_patterns(&value)),
                        options: Vec::new(),
                    },
                ));
            }
            "match" => {
                blocks.push(std::mem::replace(
                    &mut current,
                    Block {
                        condition: Condition::Match(parse_criteria(&value)),
                        options: Vec::new(),
                    },
                ));
            }
            "include" => {
                if depth >= MAX_INCLUDE_DEPTH {
                    tracing::warn!(pattern = %value, "include nesting too deep; ignoring");
                    continue;
                }
                // An include is spliced in where it appears, so first-wins ordering still holds
                // across the boundary. The block being built is flushed first for the same reason.
                blocks.push(std::mem::replace(
                    &mut current,
                    Block {
                        condition: Condition::Global,
                        options: Vec::new(),
                    },
                ));
                let included = resolve(&value);
                for contents in included {
                    parse_into(&contents, blocks, resolve, depth + 1);
                }
            }
            _ => current.options.push((keyword, value)),
        }
    }

    blocks.push(current);
}

/// Split a line into a lowercased keyword and its value.
///
/// Keywords may be separated from values by whitespace or `=`, and values may be quoted.
fn split_line(raw: &str) -> Option<(String, String)> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    // A line with no separator has no value, and a keyword without a value means nothing.
    let index = line.find(['\t', ' ', '='])?;
    let (keyword, rest) = (&line[..index], line[index + 1..].trim_start());

    // `Keyword = value` — drop a separator that survived the split.
    let rest = rest.trim_start_matches('=').trim();
    if rest.is_empty() {
        return None;
    }

    Some((keyword.to_ascii_lowercase(), unquote(rest)))
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn parse_host_patterns(value: &str) -> Vec<HostPattern> {
    value
        .split_whitespace()
        .map(|part| match part.strip_prefix('!') {
            Some(inner) => HostPattern {
                pattern: inner.to_string(),
                negated: true,
            },
            None => HostPattern {
                pattern: part.to_string(),
                negated: false,
            },
        })
        .collect()
}

/// Whether a `Host` line covers `host`.
///
/// "If a negated entry is matched, then the `Host` entry is ignored, regardless of whether any other
/// patterns on the line match" — so a negation is not merely one vote among several, it vetoes.
fn patterns_match(patterns: &[HostPattern], host: &str) -> bool {
    let mut matched = false;
    for pattern in patterns {
        if !glob_match(&pattern.pattern, host) {
            continue;
        }
        if pattern.negated {
            return false;
        }
        matched = true;
    }
    matched
}

fn parse_criteria(value: &str) -> Vec<Criterion> {
    let mut criteria = Vec::new();
    let mut tokens = value.split_whitespace().peekable();

    while let Some(token) = tokens.next() {
        let keyword = token.to_ascii_lowercase();
        match keyword.as_str() {
            "all" => criteria.push(Criterion::All),
            // Neither changes whether a block applies for us: we never canonicalise host names, and
            // there is only one pass, so both are simply no-ops rather than reasons to skip.
            "canonical" | "final" => {}
            "host" | "originalhost" | "user" | "localuser" => {
                let Some(argument) = tokens.next() else {
                    criteria.push(Criterion::Unsupported(keyword));
                    continue;
                };
                let patterns = parse_host_patterns(argument);
                criteria.push(match keyword.as_str() {
                    "host" => Criterion::Host(patterns),
                    "originalhost" => Criterion::OriginalHost(patterns),
                    "user" => Criterion::User(patterns),
                    _ => Criterion::LocalUser(patterns),
                });
            }
            other => {
                // `exec` and anything else with an argument: consume the argument so it is not
                // mistaken for a criterion of its own.
                if tokens.peek().is_some() {
                    let _ = tokens.next();
                }
                criteria.push(Criterion::Unsupported(other.to_string()));
            }
        }
    }

    criteria
}

fn match_applies(
    criteria: &[Criterion],
    context: &QueryContext<'_>,
    unsupported: &mut Vec<String>,
) -> bool {
    if criteria.is_empty() {
        return false;
    }

    let mut applies = true;
    for criterion in criteria {
        let satisfied = match criterion {
            Criterion::All => true,
            Criterion::Host(patterns) | Criterion::OriginalHost(patterns) => {
                patterns_match(patterns, context.host)
            }
            Criterion::User(patterns) => context
                .user
                .is_some_and(|user| patterns_match(patterns, user)),
            Criterion::LocalUser(patterns) => context
                .local_user
                .is_some_and(|user| patterns_match(patterns, user)),
            Criterion::Unsupported(name) => {
                unsupported.push(name.clone());
                // Refusing to match is the safe direction: a block that cannot be evaluated must not
                // silently apply settings the user expected to be conditional.
                false
            }
        };
        applies &= satisfied;
    }

    applies
}

/// `*` matches any run, `?` matches one character. Iterative, so a pathological pattern in a
/// configuration file cannot exhaust the stack.
fn glob_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0usize, 0usize);
    let (mut star, mut resume) = (None, 0usize);

    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            resume = n;
            p += 1;
        } else if let Some(star_at) = star {
            p = star_at + 1;
            resume += 1;
            n = resume;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(host: &str) -> QueryContext<'_> {
        QueryContext {
            host,
            user: None,
            local_user: None,
        }
    }

    #[test]
    fn the_first_value_wins_not_the_last() {
        // The rule the manual states and parsers routinely invert. A file organised the way the
        // manual recommends — specific first, `Host *` last — connects to the wrong port if a later
        // block is allowed to overwrite an earlier one.
        let text = "\
Host srv
    Port 2222
Host *
    Port 22
";
        let query = SshConfig::parse(text).query(&context("srv"));
        assert_eq!(query.port(), 2222);
    }

    #[test]
    fn a_global_option_before_any_host_applies_to_everything() {
        let text = "\
Compression yes
Host srv
    Port 2222
";
        let query = SshConfig::parse(text).query(&context("srv"));
        assert_eq!(query.flag("compression"), Some(true));
        assert_eq!(query.port(), 2222);
    }

    #[test]
    fn hostname_replaces_the_name_that_was_asked_for() {
        let text = "\
Host db
    HostName db-1.internal
    User ops
";
        let query = SshConfig::parse(text).query(&context("db"));
        assert_eq!(query.host_name("db"), "db-1.internal");
        assert_eq!(query.user(), Some("ops"));
    }

    #[test]
    fn an_unmatched_host_gets_only_the_defaults() {
        let text = "Host srv\n    Port 2222\n";
        let query = SshConfig::parse(text).query(&context("other"));
        assert_eq!(query.port(), 22);
        assert_eq!(query.host_name("other"), "other");
        assert_eq!(query.user(), None);
    }

    #[test]
    fn wildcards_and_multiple_patterns_on_one_line() {
        let text = "\
Host *.internal 10.0.0.?
    User ops
";
        let config = SshConfig::parse(text);
        assert_eq!(config.query(&context("db.internal")).user(), Some("ops"));
        assert_eq!(config.query(&context("10.0.0.5")).user(), Some("ops"));
        assert_eq!(config.query(&context("10.0.0.55")).user(), None);
        assert_eq!(config.query(&context("elsewhere")).user(), None);
    }

    #[test]
    fn a_negated_pattern_vetoes_the_whole_line() {
        // "If a negated entry is matched, then the Host entry is ignored, regardless of whether any
        // other patterns on the line match."
        let text = "\
Host *.internal !secret.internal
    User ops
";
        let config = SshConfig::parse(text);
        assert_eq!(config.query(&context("db.internal")).user(), Some("ops"));
        assert_eq!(config.query(&context("secret.internal")).user(), None);
    }

    #[test]
    fn identity_files_accumulate_instead_of_competing() {
        // The documented exception to first-wins.
        let text = "\
Host srv
    IdentityFile ~/.ssh/id_ed25519
Host *
    IdentityFile ~/.ssh/id_rsa
";
        let query = SshConfig::parse(text).query(&context("srv"));
        assert_eq!(
            query.identity_files(),
            &["~/.ssh/id_ed25519".to_string(), "~/.ssh/id_rsa".to_string()]
        );
    }

    #[test]
    fn keywords_are_case_insensitive_and_accept_an_equals_separator() {
        let text = "Host srv\n    PORT=2222\n    user\tops\n";
        let query = SshConfig::parse(text).query(&context("srv"));
        assert_eq!(query.port(), 2222);
        assert_eq!(query.user(), Some("ops"));
        // Lookup is case-insensitive too.
        assert_eq!(query.get("PoRt"), Some("2222"));
    }

    #[test]
    fn quoted_values_lose_their_quotes() {
        let text = "Host srv\n    User \"ops person\"\n";
        let query = SshConfig::parse(text).query(&context("srv"));
        assert_eq!(query.user(), Some("ops person"));
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let text = "\
# a comment

Host srv
    # another
    Port 2222
";
        assert_eq!(SshConfig::parse(text).query(&context("srv")).port(), 2222);
    }

    #[test]
    fn proxy_jump_is_split_into_hops_nearest_first() {
        let text = "Host target\n    ProxyJump bastion1,user@bastion2:2222\n";
        let hops = SshConfig::parse(text)
            .query(&context("target"))
            .proxy_jump();
        assert_eq!(
            hops,
            vec![
                JumpHop {
                    host: "bastion1".to_string(),
                    user: None,
                    port: None
                },
                JumpHop {
                    host: "bastion2".to_string(),
                    user: Some("user".to_string()),
                    port: Some(2222)
                },
            ]
        );
    }

    #[test]
    fn proxy_jump_none_cancels_an_inherited_chain() {
        // How a specific host opts out of a bastion set for everything else.
        let text = "\
Host direct
    ProxyJump none
Host *
    ProxyJump bastion
";
        let config = SshConfig::parse(text);
        assert!(config.query(&context("direct")).proxy_jump().is_empty());
        assert_eq!(config.query(&context("other")).proxy_jump().len(), 1);
    }

    #[test]
    fn a_jump_hop_with_an_ipv6_address_keeps_its_colons() {
        assert_eq!(
            JumpHop::parse("[2001:db8::1]:2222"),
            JumpHop {
                host: "2001:db8::1".to_string(),
                user: None,
                port: Some(2222)
            }
        );
        // Unbracketed, so the colons are part of the address and there is no port.
        assert_eq!(
            JumpHop::parse("2001:db8::1"),
            JumpHop {
                host: "2001:db8::1".to_string(),
                user: None,
                port: None
            }
        );
    }

    #[test]
    fn match_all_applies_to_everything() {
        let text = "Match all\n    Compression yes\n";
        let query = SshConfig::parse(text).query(&context("anything"));
        assert_eq!(query.flag("compression"), Some(true));
    }

    #[test]
    fn match_host_and_user_must_both_hold() {
        let text = "Match host *.internal user ops\n    Port 2222\n";
        let config = SshConfig::parse(text);

        let both = QueryContext {
            host: "db.internal",
            user: Some("ops"),
            local_user: None,
        };
        assert_eq!(config.query(&both).port(), 2222);

        let wrong_user = QueryContext {
            host: "db.internal",
            user: Some("someone"),
            local_user: None,
        };
        assert_eq!(config.query(&wrong_user).port(), 22);

        let wrong_host = QueryContext {
            host: "elsewhere",
            user: Some("ops"),
            local_user: None,
        };
        assert_eq!(config.query(&wrong_host).port(), 22);
    }

    #[test]
    fn match_localuser_reads_the_local_account() {
        let text = "Match localuser root\n    User admin\n";
        let config = SshConfig::parse(text);
        let as_root = QueryContext {
            host: "h",
            user: None,
            local_user: Some("root"),
        };
        assert_eq!(config.query(&as_root).user(), Some("admin"));
        assert_eq!(config.query(&context("h")).user(), None);
    }

    #[test]
    fn match_exec_never_applies_and_is_reported() {
        // Running a command out of a config file just to list someone's hosts is not a default worth
        // having. The block is skipped and the caller is told, rather than being left to wonder why
        // a setting did not take.
        let text = "Match exec \"true\"\n    Port 2222\n";
        let query = SshConfig::parse(text).query(&context("h"));
        assert_eq!(query.port(), 22);
        assert_eq!(query.unsupported, vec!["exec".to_string()]);
    }

    #[test]
    fn canonical_and_final_do_not_stop_a_block_applying() {
        let text = "Match final host srv\n    Port 2222\n";
        let query = SshConfig::parse(text).query(&context("srv"));
        assert_eq!(query.port(), 2222);
        assert!(query.unsupported.is_empty());
    }

    #[test]
    fn include_is_spliced_in_where_it_appears() {
        // And therefore participates in first-wins ordering.
        let main = "\
Host srv
    Port 2222
Include extra
Host *
    Port 22
";
        let mut resolve = |pattern: &str| {
            assert_eq!(pattern, "extra");
            vec!["Host srv\n    User from-include\n".to_string()]
        };
        let config = SshConfig::parse_with_includes(main, &mut resolve);
        let query = config.query(&context("srv"));
        assert_eq!(query.port(), 2222);
        assert_eq!(query.user(), Some("from-include"));
    }

    #[test]
    fn an_earlier_value_still_beats_one_from_a_later_include() {
        let main = "Host srv\n    Port 2222\nInclude later\n";
        let mut resolve = |_: &str| vec!["Host srv\n    Port 9999\n".to_string()];
        let config = SshConfig::parse_with_includes(main, &mut resolve);
        assert_eq!(config.query(&context("srv")).port(), 2222);
    }

    #[test]
    fn several_included_files_are_read_in_the_order_given() {
        let main = "Include conf.d/*\n";
        let mut resolve = |_: &str| {
            vec![
                "Host srv\n    Port 1111\n".to_string(),
                "Host srv\n    Port 2222\n".to_string(),
            ]
        };
        let config = SshConfig::parse_with_includes(main, &mut resolve);
        assert_eq!(
            config.query(&context("srv")).port(),
            1111,
            "lexical order, and first wins"
        );
    }

    #[test]
    fn a_self_including_file_stops_instead_of_looping() {
        let mut depth = 0usize;
        {
            let mut resolve = |_: &str| {
                depth += 1;
                vec!["Include self\n".to_string()]
            };
            // Must return rather than recurse forever.
            let _ = SshConfig::parse_with_includes("Include self\n", &mut resolve);
        }
        assert!(depth <= MAX_INCLUDE_DEPTH + 1, "recursed {depth} times");
    }

    #[test]
    fn an_empty_file_yields_defaults() {
        let query = SshConfig::parse("").query(&context("h"));
        assert_eq!(query.port(), 22);
        assert!(query.identity_files().is_empty());
        assert!(query.proxy_jump().is_empty());
    }

    #[test]
    fn a_keyword_with_no_value_is_ignored_rather_than_crashing() {
        let text = "Host srv\n    Port\n    User ops\n";
        let query = SshConfig::parse(text).query(&context("srv"));
        assert_eq!(query.port(), 22);
        assert_eq!(query.user(), Some("ops"));
    }

    #[test]
    fn flags_read_both_spellings() {
        let text = "Host srv\n    ForwardAgent yes\n    Compression no\n";
        let query = SshConfig::parse(text).query(&context("srv"));
        assert_eq!(query.flag("forwardagent"), Some(true));
        assert_eq!(query.flag("compression"), Some(false));
        assert_eq!(query.flag("neverset"), None);
    }
}
