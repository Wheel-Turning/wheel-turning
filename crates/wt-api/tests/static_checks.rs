//! Spec 06 checks that need no browser and no database: the measured contrast table computed
//! from the stylesheet tokens, bright gold kept off light surfaces, no external hosts or fonts,
//! a viewport that never blocks zoom, and reduced motion.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::collections::HashMap;
use std::path::Path;

const CSS: &str = include_str!("../static/app.css");

/// The `--name: #RRGGBB` tokens in `:root`.
fn tokens() -> HashMap<String, (f64, f64, f64)> {
    let root = &CSS[CSS.find(":root {").unwrap() + ":root {".len()..];
    let root = &root[..root.find('}').unwrap()];
    root.split(';')
        .filter_map(|decl| {
            let (name, value) = decl.split_once(':')?;
            let hex = value.trim().strip_prefix('#')?;
            let n = u32::from_str_radix(hex, 16).ok()?;
            let channel = |shift: u32| f64::from((n >> shift) & 0xff) / 255.0;
            Some((
                name.trim().trim_start_matches("--").to_owned(),
                (channel(16), channel(8), channel(0)),
            ))
        })
        .collect()
}

/// WCAG 2.2 relative luminance.
fn luminance((r, g, b): (f64, f64, f64)) -> f64 {
    let lin = |c: f64| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

fn ratio(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[test]
fn every_pair_in_the_measured_contrast_table_meets_its_threshold() {
    let t = tokens();
    // (foreground, background, threshold, the ratio Spec 06 measured)
    let pairs = [
        ("ink", "paper", 4.5, 15.5),
        ("ink", "band", 4.5, 14.1),
        ("muted", "paper", 4.5, 8.1),
        ("muted", "band", 4.5, 7.4),
        ("paper", "ink", 4.5, 15.5),
        ("error", "paper", 4.5, 7.6),
        ("visited", "paper", 4.5, 7.9),
        ("on-dark", "dark", 4.5, 14.7),
        ("on-dark-muted", "dark", 4.5, 11.8),
        ("control", "paper", 3.0, 4.7),
        ("gold-deep", "paper", 3.0, 3.5),
        ("gold", "dark", 3.0, 8.2),
    ];
    for (fg, bg, threshold, measured) in pairs {
        let r = ratio(t[fg], t[bg]);
        assert!(r >= threshold, "{fg} on {bg}: {r:.2} is below {threshold}");
        assert!(
            (r - measured).abs() < 0.15,
            "{fg} on {bg}: {r:.2}, the spec measured {measured}"
        );
    }
}

#[test]
fn bright_gold_never_appears_on_a_light_surface() {
    // `--gold` is used only on the dark footer; `--gold-deep` is the light-surface gold.
    for rule in CSS.split('}') {
        if rule.contains("var(--gold)") {
            let selector = rule.split('{').next().unwrap_or_default();
            assert!(
                selector.contains("footer.site"),
                "bright gold outside the footer: {rule}"
            );
        }
    }
    let t = tokens();
    assert!(
        ratio(t["gold"], t["paper"]) < 3.0,
        "the reason for the rule"
    );
}

fn files(dir: &Path) -> Vec<(String, String)> {
    std::fs::read_dir(dir)
        .unwrap()
        .flat_map(|e| {
            let path = e.unwrap().path();
            if path.is_dir() {
                files(&path)
            } else {
                vec![(
                    path.display().to_string(),
                    std::fs::read_to_string(&path).unwrap(),
                )]
            }
        })
        .collect()
}

#[test]
fn no_template_or_stylesheet_references_an_external_host_or_font() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut all = files(&root.join("templates"));
    all.extend(files(&root.join("static")));
    for (path, text) in all {
        // The SVG namespace is an identifier, not a request, and a link the reader follows
        // loads nothing (the support notices link to 988lifeline.org and findahelpline.com).
        let text = text
            .replace("http://www.w3.org/2000/svg", "")
            .replace("<a href=\"https://", "<a href=\"");
        for needle in ["http://", "https://", "//fonts.", "@import", "@font-face"] {
            assert!(!text.contains(needle), "{path} contains {needle}");
        }
        assert!(
            !text.contains("src=\"//"),
            "{path} has a protocol-relative source"
        );
    }
}

#[test]
fn the_viewport_never_blocks_zoom_and_scripts_are_not_inline() {
    let base = include_str!("../templates/base.html");
    let viewport = base
        .lines()
        .find(|l| l.contains("name=\"viewport\""))
        .unwrap();
    assert!(viewport.contains("width=device-width, initial-scale=1"));
    assert!(!viewport.contains("user-scalable") && !viewport.contains("maximum-scale"));
    for line in base.lines().filter(|l| l.contains("<script")) {
        assert!(line.contains("src=\"/static/"), "inline script: {line}");
    }
    assert!(base.contains("lang=\"en\""));
}

#[test]
fn reduced_motion_turns_the_menu_animation_off() {
    assert!(CSS.contains("animation: wt-slide 150ms"));
    let reduced = &CSS[CSS.find("prefers-reduced-motion: reduce").unwrap()..];
    assert!(reduced.contains("animation: none !important"));
}
