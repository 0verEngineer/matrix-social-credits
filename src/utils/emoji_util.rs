use crate::data::emoji::find_all_emoji_for_room_in_db;
use crate::data::user::HtmlAndTextAnswer;
use crate::utils::message::{escape_html, heading};
use matrix_sdk::Room;
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

/// Unicode variation selectors. VS16 asks for the coloured emoji presentation, VS15 for the
/// monochrome text presentation. Clients disagree on whether to send them, so the same emoji
/// arrives with and without.
const VARIATION_SELECTOR_15: char = '\u{fe0e}';
const VARIATION_SELECTOR_16: char = '\u{fe0f}';

/// Emoji modifiers Fitzpatrick-1 through Fitzpatrick-6 (skin tones).
const SKIN_TONE_RANGE: std::ops::RangeInclusive<char> = '\u{1f3fb}'..='\u{1f3ff}';

/// Bring an emoji into the single form used both when registering it and when looking it up
/// for a reaction.
///
/// Registration did not normalize at all while the lookup stripped VS16 -- and only when the
/// string ended with it, though it then removed every occurrence. So an admin whose client
/// sends "😑\u{fe0f}" registered an entry that could never be found again.
///
/// Skin tone modifiers are stripped as well: 👍 and 👍🏽 are the same reaction as far as the
/// score is concerned, and requiring a separate registration per skin tone would just mean
/// that most people's reactions silently do nothing.
pub fn normalize_emoji(emoji: &str) -> String {
    emoji
        .trim()
        .chars()
        .filter(|c| {
            *c != VARIATION_SELECTOR_15
                && *c != VARIATION_SELECTOR_16
                && !SKIN_TONE_RANGE.contains(c)
        })
        .collect()
}

pub fn get_emoji_list_answer(conn: &Arc<Mutex<Connection>>, room: &Room) -> HtmlAndTextAnswer {
    let emojis_opt = find_all_emoji_for_room_in_db(conn, &room.room_id().to_string());
    let empty_answer = HtmlAndTextAnswer {
        html: String::from("No emojis, use the !help command to see how to add emojis"),
        text: String::from("No emojis, use the !help command to see how to add emojis"),
    };

    if emojis_opt.is_none() {
        return empty_answer;
    }

    let mut emojis = emojis_opt.unwrap();

    if emojis.is_empty() {
        return empty_answer;
    }

    // Sort emojis by social credit
    emojis.sort_by(|a, b| {
        b.social_credit
            .cmp(&a.social_credit)
            .then_with(|| a.emoji.cmp(&b.emoji))
    });

    let text_entries: Vec<String> = emojis
        .iter()
        .map(|emoji| format!("{}: {}", emoji.emoji, emoji.social_credit))
        .collect();
    let html_entries: Vec<String> = emojis
        .iter()
        .map(|emoji| {
            format!(
                "{}: <b>{}</b>",
                escape_html(&emoji.emoji),
                emoji.social_credit
            )
        })
        .collect();

    HtmlAndTextAnswer {
        text: format!("Registered Emojis: {}", text_entries.join(", ")),
        html: format!(
            "{}{}",
            heading("Registered Emojis:"),
            html_entries.join("<br>")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_emoji;

    #[test]
    fn leaves_a_plain_emoji_alone() {
        assert_eq!(normalize_emoji("😑"), "😑");
    }

    #[test]
    fn strips_a_trailing_variation_selector() {
        assert_eq!(normalize_emoji("😑\u{fe0f}"), "😑");
    }

    /// The old lookup only stripped VS16 when the string *ended* with it, so a selector in
    /// the middle of a sequence survived and the entry never matched.
    #[test]
    fn strips_a_variation_selector_in_the_middle() {
        assert_eq!(
            normalize_emoji("\u{2764}\u{fe0f}\u{200d}\u{1f525}"),
            "\u{2764}\u{200d}\u{1f525}"
        );
    }

    #[test]
    fn strips_the_text_presentation_selector() {
        assert_eq!(normalize_emoji("\u{2764}\u{fe0e}"), "\u{2764}");
    }

    #[test]
    fn strips_skin_tone_modifiers() {
        assert_eq!(normalize_emoji("👍🏽"), "👍");
        assert_eq!(normalize_emoji("👍🏻"), normalize_emoji("👍🏿"));
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(normalize_emoji("  😑 "), "😑");
    }

    /// Registration and lookup have to agree; this is the case that used to silently produce
    /// an entry that could never be found again.
    #[test]
    fn registration_and_lookup_agree() {
        assert_eq!(normalize_emoji("😑\u{fe0f}"), normalize_emoji("😑"));
    }

    #[test]
    fn an_empty_input_stays_empty() {
        assert_eq!(normalize_emoji("   "), "");
    }
}
