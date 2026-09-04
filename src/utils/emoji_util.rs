use std::sync::{Arc, Mutex};
use matrix_sdk::Room;
use rusqlite::Connection;
use crate::data::emoji::find_all_emoji_for_room_in_db;
use crate::data::user::{HtmlAndTextAnswer};

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

    let mut text_body = String::from("Registered Emojis: ");
    let mut html_body = String::from("<h3>Registered Emojis:</h3><br>");

    let mut emojis = emojis_opt.unwrap();

    if emojis.len() == 0 {
        return empty_answer;
    }

    // Sort emojis by social credit
    emojis.sort_by(|a, b| {
        let a_credit = a.social_credit;
        let b_credit = b.social_credit;
        b_credit.cmp(&a_credit)
    });

    for emoji in emojis {
        text_body.push_str(&format!("{}: {},", emoji.emoji, emoji.social_credit));
        html_body.push_str(&format!("{}: <b>{}</b><br>", emoji.emoji, emoji.social_credit));
    }

    // Remove the last comma
    if text_body.len() >= 1 {
        text_body.remove(text_body.len() - 1);
    }
    // Remove the last <br>
    if html_body.len() >= 4 {
        html_body.truncate(html_body.len() - 4);
    }

    HtmlAndTextAnswer {
        html: html_body.to_string(),
        text: text_body.to_string(),
    }
}
