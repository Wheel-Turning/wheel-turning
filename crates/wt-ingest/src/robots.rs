//! A small `robots.txt` reader: the group for our user agent, or `*`, and its `Allow` and
//! `Disallow` prefixes, longest match winning. Enough to obey a site's rules before reading
//! a page; it does not handle wildcards beyond a trailing `$` or `*`, and treats an unhandled
//! wildcard rule as disallowing its literal prefix, which errs toward not fetching.

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
                        let prefix = value
                            .trim_end_matches('$')
                            .split('*')
                            .next()
                            .unwrap_or_default();
                        rules.push((key == "allow", prefix.to_owned()));
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

    /// Whether `path` (with its query) may be fetched. The longest matching prefix decides,
    /// and `Allow` wins a tie.
    pub fn allows(&self, path: &str) -> bool {
        self.rules
            .iter()
            .filter(|(_, prefix)| path.starts_with(prefix.as_str()))
            .max_by_key(|(allow, prefix)| (prefix.len(), *allow))
            .is_none_or(|(allow, _)| *allow)
    }
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
    fn empty_or_missing_rules_allow_everything_and_disallow_all_blocks_all() {
        assert!(Robots::parse("", "x").allows("/anything"));
        assert!(Robots::parse("User-agent: *\nDisallow:", "x").allows("/anything"));
        assert!(!Robots::parse("User-agent: *\nDisallow: /", "x").allows("/talks-page/"));
    }
}
