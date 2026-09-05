# Improvements

Ideas and known gaps that are not bugs. Nothing here is scheduled; the point is that the
analysis is written down, so the next person does not have to redo it before deciding whether
something is worth building.

Fixed problems live in [`CHANGELOG.md`](CHANGELOG.md), current shortcomings that users should
know about live under *Limitations* in the [README](README.md).

| # | Idea | Size | Why it matters |
| --- | --- | --- | --- |
| 1 | [Device verification](#1-device-verification) | medium | The bot cannot read messages from anyone who restricts keys to verified sessions |
| 2 | [A web interface](#2-a-web-interface) | large | Everything is configured through environment variables and chat commands today |
| 3 | [Room data for every member on join](#3-room-data-for-every-member-on-join) | small | People only appear in `!list` once they have sent something |
| 4 | [Per-room configuration](#4-per-room-configuration) | medium | Cooldown and starting score are global |
| 5 | [More than one admin](#5-more-than-one-admin) | small | Exactly one, and only through an environment variable |
| 6 | [Undo a score change when the reaction is removed](#6-undo-a-score-change-when-the-reaction-is-removed) | small | A misclick is permanent |
| 7 | [Smaller cleanups](#7-smaller-cleanups) | small | Things noticed during the review that were not worth a fix on their own |

---

## 1. Device verification

The bot runs as an unverified session. That is fine for most people, but anybody whose client
is set to share room keys only with verified sessions will not share them with the bot, and
their messages stay unreadable for it. Their reactions still count.

### A web interface is not needed for this

Worth stating up front, because it is the obvious assumption: emoji verification does not need
a login page anywhere. The SAS ("short authentication string") flow runs over Matrix itself,
as to-device or in-room events, and `matrix-sdk` exposes all of it. The seven emojis can be
posted into the room, and the confirmation can be a chat command.

The only thing the bot cannot do is decide by itself whether the emojis match — that is the
entire security property. It needs one input from a human, and a command is a perfectly good
way to get it.

### Sketch

The pieces exist in `matrix-sdk` 0.18 (`matrix_sdk::encryption::verification`):

```rust
// 1. The admin starts the verification in their client. The bot receives
//    m.key.verification.request, either as a to-device event or in the room.
let request = client.encryption().get_verification_request(user_id, flow_id).await?;
request.accept().await?;

// 2. One side starts SAS; the bot can do it, or accept the one the client started.
let Verification::SasV1(sas) = client.encryption().get_verification(user_id, flow_id).await? else { ... };
sas.accept().await?;

// 3. Once both sides have exchanged keys, the emojis are available.
if sas.can_be_presented() {
    let emojis = sas.emoji().unwrap();  // [Emoji; 7], each with .symbol and .description
    // post them into the room
}

// 4. The admin compares them with what their client shows.
//    !verify yes  -> sas.confirm().await?
//    !verify no   -> sas.mismatch().await?
```

### What to think about before building it

- **Who may verify.** Only the configured admin, and only in a direct message — not in a
  group room where everybody can read along and confirm.
- **A timeout.** A verification left half-finished should be cancelled (`sas.cancel()`),
  otherwise the next attempt runs into a flow that is still open.
- **Cross-signing.** Verifying the device against one person's session is the small version.
  The complete version is for the bot to have its own cross-signing identity, so it is
  verified for everybody at once instead of per session. Bigger job, and it needs a place to
  keep the cross-signing keys.
- **It does not fix the past.** Verification only affects keys shared from that moment on.
  Messages that were unreadable stay unreadable.

---

## 2. A web interface

Not needed for verification (see above), but there are things chat commands are a poor fit
for: configuring the cooldown and the starting score per room, correcting a score by hand,
managing emojis with more comfort than `!register_emoji`, looking at a history, or seeing at a
glance which rooms the bot is even in.

The hard part is not the interface. It is the login.

### How people could log in

Four options, from "works today" to "do not do this".

#### a) Matrix OpenID token — works with a plain Synapse, no server configuration

Matrix has a mechanism for exactly this: proving to a third-party service who you are on
Matrix, without that service ever seeing your password.

```
1. The user's client asks their homeserver for a short-lived token:
   POST /_matrix/client/v3/user/{userId}/openid/request_token
   -> { "access_token": "...", "matrix_server_name": "example.org", "expires_in": 3600 }

2. The user hands that token to the web interface.

3. The web interface asks that homeserver who the token belongs to:
   GET https://example.org/_matrix/federation/v1/openid/userinfo?access_token=...
   -> { "sub": "@alice:example.org" }
```

Step 3 is unauthenticated and needs no relationship with the homeserver whatsoever, which is
what makes this work without configuring anything on the Synapse side.

The catch is step 2: the user has to be logged in with a Matrix client to obtain the token in
the first place. For a standalone web page that means copy and paste, which nobody enjoys.
Which leads to:

#### b) A widget inside Element — the same mechanism, without the copy and paste

A widget is a web page embedded in a Matrix room. Element hands it an OpenID token through the
widget API — the same token as above, obtained for the user automatically. From the user's
point of view there is no login at all: they open the bot's panel in the room and are already
identified.

Best user experience of the four, and it fits a room bot: the configuration lives where the
bot lives. The cost is that the interface has to be registered as a widget in the room (an
`im.vector.modular.widgets` state event, so it needs a power level), it has to speak the
widget API, and outside Element the support varies.

#### c) OIDC through Matrix Authentication Service — the future-proof one, if you run MAS

[Matrix Authentication Service](https://github.com/matrix-org/matrix-authentication-service) is
a real OAuth 2.0 / OpenID Connect provider for Synapse, and the direction Matrix
authentication is moving in (MSC3861). Where it is deployed, a web interface can do an
ordinary authorization code flow against it and get back a verified Matrix user id — the same
"log in with …" you know from anywhere else, with a proper redirect and consent screen.

The condition is in the first sentence: the deployment has to run MAS. A plain Synapse with
local passwords does not have it. Worth designing for, not worth waiting for.

#### d) Asking for the Matrix password in the bot's own form — no

It would be the shortest path and it is the wrong one. It trains people to type their Matrix
password into whatever asks for it, and it puts credentials for the whole account into a bot
whose job is counting emojis. If a login form is unavoidable, it belongs to the homeserver,
not here.

### Recommendation

If this ever gets built: (b) as a widget, with (a) as the fallback for people who want to open
the page outside Element, because both rest on the same token and the second one is then
nearly free. (c) once MAS is common enough to assume.

---

## 3. Room data for every member on join

The bot creates a user's room data the first time it sees an event from them. Somebody who is
in the room but has never written anything and never reacted does not exist for the bot, so
they are missing from `!list` even though they are a member.

Fetching the member list when the bot joins a room, and reacting to `m.room.member` join
events afterwards, would fix it. Note that `!list` already filters against the live member
list, so nobody would be listed who is not there.

Watch out for large rooms: this creates one row per member.

---

## 4. Per-room configuration

`INITIAL_SOCIAL_CREDIT`, `REACTION_LIMIT` and `REACTION_TIMESPAN` are environment variables and
therefore the same everywhere. A room where the bot is a running joke wants different numbers
from one where people take it half seriously.

This wants a `room_config` table with the environment variables as defaults, and either
commands to change it or the interface from idea 2.

---

## 5. More than one admin

There is exactly one admin, set through `ADMIN_USERNAME`, and no way to promote anybody. The
`user_type` column already knows `Moderator` and `Admin` — the enum has been there from the
start, only nothing ever writes it. A `!promote` command restricted to the configured admin
would be most of the work.

---

## 6. Undo a score change when the reaction is removed

Removing a reaction leaves the score where it is. Matrix sends a redaction for the reaction
event, so the bot could see it.

Not as small as it looks: `user_reaction` would have to remember which score change belonged
to which reaction so it can be reversed, and there would need to be a decision about whether
removing and re-adding a reaction lets somebody spend the same cooldown slot twice.

---

## 7. Smaller cleanups

Noticed during the review, none of them worth a fix on their own:

- **`session.json` is briefly world-readable.** It is written and then set to `0600`, so there
  is a short window with the default permissions. Creating it with the right mode in the first
  place would close it. It lives in a container volume that only one user can reach, so this
  is theory rather than practice.
- **The score change and its reaction row are two statements.** `add_social_credit` runs, then
  the `user_reaction` row is written. If the second one fails the score has already moved, and
  the same message could be scored again. One transaction around both would be tidier.
- **An unexpectedly new schema only produces a warning.** If `PRAGMA user_version` is higher
  than the build expects — after a rollback to an older image, say — the bot logs a warning and
  carries on against a schema it does not know. Refusing to start would be safer.
- **The cooldown message is posted into the room.** It only appears when the reaction would
  actually have counted, so it is much rarer than it used to be. If it still gets in the way:
  drop it silently, or send it once per user per cooldown window instead of every time.
