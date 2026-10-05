//! A small `robots.txt` reader (RFC 9309): the group for our user agent, or `*`, and its
//! `Allow` and `Disallow` rules. A rule matches from the start of the path; `*` matches any
//! run of characters and a trailing `$` anchors the end. The longest matching rule decides,
//! and `Allow` wins a tie. Enough to obey a site's rules before reading a page.

/// The rules that apply to one user agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Robots {
    rules: Vec<(bool, String)>,
}

impl Robots {
    /// Parses `robots.txt` for `agent` (matched case-insensitively against the start of each
    /// `User-agent` value). The specific group wins over `*`.
    pub fn parse(text: &str, agent: &str) -> Self {
        let agent = agent.to_ascii_lowercase();
        let mut specific: Option<Vec<(bool, String)>> = None;
        let mut star: Option<Vec<(bool, String)>> = None;
        let mut agents: Vec<String> = Vec::new();
        let mut rules: Vec<(bool, String)> = Vec::new();
        let mut in_rules = false;

        let mut flush = |agents: &mut Vec<String>, rules: &mut Vec<(bool, String)>| {
            for a in agents.iter() {
                if a == "*" {
                    star.get_or_insert_with(Vec::new)
                        .extend(rules.iter().cloned());
                } else if agent.starts_with(a.as_str()) {
                    specific
                        .get_or_insert_with(Vec::new)
                        .extend(rules.iter().cloned());
                }
            }
            agents.clear();
            rules.clear();
        };

        for line in text.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            match key.as_str() {
                "user-agent" => {
                    if in_rules {
                        flush(&mut agents, &mut rules);
                        in_rules = false;
                    }
                    agents.push(value.to_ascii_lowercase());
                }
                "allow" | "disallow" => {
                    in_rules = true;
                    if !value.is_empty() {
                        rules.push((key == "allow", value.to_owned()));
                    }
                }
                _ => {}
            }
        }
        flush(&mut agents, &mut rules);
        Robots {
            rules: specific.or(star).unwrap_or_default(),
        }
    }

    /// Whether `path` (with its query) may be fetched. The longest matching rule decides,
    /// and `Allow` wins a tie.
    pub fn allows(&self, path: &str) -> bool {
        self.rules
            .iter()
            .filter(|(_, pattern)| matches(pattern, path))
            .max_by_key(|(allow, pattern)| (pattern.len(), *allow))
            .is_none_or(|(allow, _)| *allow)
    }
}

/// Whether a rule matches `path` from its start. `*` matches any run of characters, including
/// none, and a trailing `$` means the path must end where the pattern does.
fn matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let mut pieces = pattern.split('*');
    let first = pieces.next().unwrap_or_default();
    let Some(mut rest) = path.strip_prefix(first) else {
        return false;
    };
    let pieces: Vec<&str> = pieces.collect();
    for (i, piece) in pieces.iter().enumerate() {
        let last = i + 1 == pieces.len();
        if last && anchored {
            return rest.ends_with(piece);
        }
        match rest.find(piece) {
            Some(at) => rest = &rest[at + piece.len()..],
            None => return false,
        }
    }
    !anchored || rest.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROBOTS: &str = "
        # comments are ignored
        User-agent: *
        Disallow: /wp-admin/
        Allow: /wp-admin/admin-ajax.php
        Disallow: /private

        User-agent: WheelTurning
        Disallow: /talks-page/hidden
    ";

    #[test]
    fn the_specific_group_wins_and_longest_prefix_decides() {
        let ours = Robots::parse(ROBOTS, "WheelTurning/0.1");
        assert!(ours.allows("/talks-page/"));
        assert!(!ours.allows("/talks-page/hidden/1"));
        assert!(
            ours.allows("/private"),
            "the * group does not apply when ours exists"
        );

        let other = Robots::parse(ROBOTS, "SomeBot");
        assert!(!other.allows("/wp-admin/options.php"));
        assert!(other.allows("/wp-admin/admin-ajax.php"));
        assert!(!other.allows("/private/x"));
        assert!(other.allows("/talks-page/"));
    }

    #[test]
    fn wildcards_match_inside_the_path_and_never_widen_to_the_whole_site() {
        // Rules like a WordPress shop's: the wildcard rules must block cart links only.
        let shop = Robots::parse(
            "User-agent: *
Disallow: /*?add-to-cart=
Disallow: /*?*add-to-cart=
             Disallow: /wp-admin/
Allow: /wp-admin/admin-ajax.php
",
            "WheelTurning/0.1",
        );
        assert!(shop.allows("/talks-page/"));
        assert!(shop.allows("/"));
        assert!(!shop.allows("/shop/?add-to-cart=12"));
        assert!(!shop.allows("/shop/?size=2&add-to-cart=12"));
        assert!(shop.allows("/shop/?size=2"));

        let anchored = Robots::parse(
            "User-agent: *
Disallow: /*.pdf$
Disallow: /exact$
",
            "x",
        );
        assert!(!anchored.allows("/files/notes.pdf"));
        assert!(anchored.allows("/files/notes.pdf?download=1"));
        assert!(!anchored.allows("/exact"));
        assert!(anchored.allows("/exact/more"));
    }

    #[test]
    fn empty_or_missing_rules_allow_everything_and_disallow_all_blocks_all() {
        assert!(Robots::parse("", "x").allows("/anything"));
        assert!(Robots::parse("User-agent: *\nDisallow:", "x").allows("/anything"));
        assert!(!Robots::parse("User-agent: *\nDisallow: /", "x").allows("/talks-page/"));
    }
}
