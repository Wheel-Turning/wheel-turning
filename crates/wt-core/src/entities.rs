//! HTML character references left in stored text. Some source pages put typographic quotes and
//! dashes in a transcript as `&ldquo;` or `&#8217;`, and the ingest kept them as written. Stored
//! spans are character offsets into that text, so it is not rewritten: the page decodes each
//! piece it shows instead.

use std::borrow::Cow;

/// The named references these pages use, with the characters they stand for.
const NAMED: [(&str, char); 22] = [
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", ' '),
    ("ldquo", '\u{201C}'),
    ("rdquo", '\u{201D}'),
    ("lsquo", '\u{2018}'),
    ("rsquo", '\u{2019}'),
    ("sbquo", '\u{201A}'),
    ("bdquo", '\u{201E}'),
    ("laquo", '\u{00AB}'),
    ("raquo", '\u{00BB}'),
    ("mdash", '\u{2014}'),
    ("ndash", '\u{2013}'),
    ("hellip", '\u{2026}'),
    ("frasl", '\u{2044}'),
    ("eacute", '\u{00E9}'),
    ("egrave", '\u{00E8}'),
    ("uuml", '\u{00FC}'),
    ("ouml", '\u{00F6}'),
];

/// The longest reference read, `&` and `;` included.
const LONGEST: usize = 10;

/// The character `body` (between `&` and `;`) stands for, if it is a reference this decodes.
fn reference(body: &str) -> Option<char> {
    if let Some(num) = body.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse::<u32>().ok()?,
        };
        return char::from_u32(code).filter(|c| !c.is_control() || c.is_whitespace());
    }
    NAMED.iter().find(|(n, _)| *n == body).map(|(_, c)| *c)
}

/// `text` with the character references it holds decoded; anything else, an unknown name or a
/// bare `&` included, stays as it is.
pub fn decode(text: &str) -> Cow<'_, str> {
    if !text.contains('&') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let decoded = tail
            .char_indices()
            .take(LONGEST)
            .find(|(_, c)| *c == ';')
            .and_then(|(end, _)| reference(&tail[1..end]).map(|c| (c, end + 1)));
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_and_numeric_references_decode() {
        assert_eq!(
            decode("he said &ldquo;sit&rdquo; &mdash; it&rsquo;s &#8216;so&#x2019;&hellip;"),
            "he said \u{201C}sit\u{201D} \u{2014} it\u{2019}s \u{2018}so\u{2019}\u{2026}"
        );
    }

    #[test]
    fn anything_else_stays_as_written() {
        assert_eq!(decode("Tom & Jerry"), "Tom & Jerry");
        assert_eq!(decode("&unknown; &#xZZ; &#0; &"), "&unknown; &#xZZ; &#0; &");
        assert_eq!(decode("&ampersand without end"), "&ampersand without end");
        assert!(matches!(decode("plain"), Cow::Borrowed(_)));
    }
}
