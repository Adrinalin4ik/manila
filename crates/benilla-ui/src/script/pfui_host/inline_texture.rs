//! The `|T…|t` inline-texture escape's byte spans, for the callers that must not draw one: an
//! EditBox shows its markup raw, and text another player typed never draws art. The stock decoder
//! ([`crate::markup`]) has no such token, so the scan rides its token walk: an escaped pipe
//! (`||T`) and an escape with no closing `|t` are ordinary text.

use std::borrow::Cow;
use std::ops::Range;

/// Each well-formed escape of `text`, `|T` through `|t`.
pub fn spans(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if !text.contains("|T") {
        return out;
    }
    let mut at = 0usize;
    while let Some(token) = crate::markup::token_at(text, at) {
        let rest = &text[at..];
        match rest
            .starts_with("|T")
            .then(|| rest[2..].find("|t"))
            .flatten()
        {
            Some(end) if end > 0 => {
                out.push(at..at + end + 4);
                at += end + 4;
            }
            _ => at += token.byte_len,
        }
    }
    out
}

/// `text` with each escape's opening pipe doubled, so it draws as the bytes it is written with.
pub fn raw(text: &str) -> Cow<'_, str> {
    let spans = spans(text);
    if spans.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + spans.len());
    let mut copied = 0usize;
    for span in spans {
        out.push_str(&text[copied..span.start]);
        out.push('|');
        copied = span.start;
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

/// `text` without its escapes.
pub fn strip(text: &str) -> Cow<'_, str> {
    let spans = spans(text);
    if spans.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize;
    for span in spans {
        out.push_str(&text[copied..span.start]);
        copied = span.end;
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_follow_the_token_walk() {
        assert_eq!(spans("a|Tfoo:16|tb"), vec![1..11]);
        assert_eq!(spans("|Ta|t|Tb|t"), vec![0..5, 5..10]);
        for text in ["a||Tfoo|t", "a|Tfoo:16", "a|T|tb", "plain"] {
            assert!(spans(text).is_empty(), "{text:?}");
        }
    }

    #[test]
    fn raw_text_draws_the_bytes_and_stripped_text_drops_them() {
        assert_eq!(raw("a|Tfoo:16|tb"), "a||Tfoo:16|tb");
        assert!(spans(&raw("a|Tfoo:16|tb|Tc|t")).is_empty());
        assert_eq!(strip("a|Tfoo:16|tb|Tc|t!"), "ab!");
        assert!(matches!(raw("a||Tfoo|t"), Cow::Borrowed(_)));
        assert!(matches!(strip("plain"), Cow::Borrowed(_)));
    }
}
