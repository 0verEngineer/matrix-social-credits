use std::sync::{Arc, Mutex};
use matrix_sdk::room::Joined;
use rusqlite::Connection;
use crate::data::emoji::find_all_emoji_for_room_in_db;
use crate::data::user::{HtmlAndTextAnswer};

pub fn get_emoji_list_answer(conn: &Arc<Mutex<Connection>>, room: &Joined) -> HtmlAndTextAnswer {
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
