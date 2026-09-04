//! Building the messages the bot posts.
//!
//! Two things were wrong with how these were assembled before: the HTML body was passed as
//! the plaintext body as well (so clients without HTML rendering, and push notifications,
//! showed raw `<b>` tags), and interpolated values were never escaped.

use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;

/// Escape the five characters that matter inside an HTML body.
///
/// Names and registered "emojis" are arbitrary text -- a registered emoji is whatever the
/// admin typed -- and went into the HTML body unescaped.
pub fn escape_html(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());

    for character in input.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }

    escaped
}

/// A formatted bot answer.
///
/// `m.notice` rather than `m.text`: that is the convention for automated messages, it keeps
/// other bots from reacting to it, and clients render it less prominently.
pub fn notice_html(plain: impl Into<String>, html: impl Into<String>) -> RoomMessageEventContent {
    RoomMessageEventContent::notice_html(plain, html)
}

/// A plain bot answer.
pub fn notice_plain(plain: impl Into<String>) -> RoomMessageEventContent {
    RoomMessageEventContent::notice_plain(plain)
}

#[cfg(test)]
mod tests {
    use super::escape_html;

    #[test]
    fn escapes_the_markup_characters() {
        assert_eq!(escape_html("<b>&\"'"), "&lt;b&gt;&amp;&quot;&#39;");
    }

    #[test]
    fn leaves_ordinary_text_and_emoji_alone() {
        assert_eq!(escape_html("alice 😑"), "alice 😑");
    }

    /// A registered "emoji" is whatever the admin typed, so it can contain markup.
    #[test]
    fn escapes_a_registered_value_containing_markup() {
        assert_eq!(escape_html("<img src=x>"), "&lt;img src=x&gt;");
    }
}
