# Improvements

Ideas and known gaps that are not bugs. Nothing here is scheduled; the point is that the
analysis is written down, so the next person does not have to redo it before deciding whether
something is worth building.

Fixed problems live in [`CHANGELOG.md`](CHANGELOG.md), current shortcomings that users should
know about live under *Limitations* in the [README](README.md).

| # | Idea | Size | Why it matters |
| --- | --- | --- | --- |
| 1 | [Device verification](#1-device-verification) | medium | The bot cannot read messages from anyone who restricts keys to verified sessions. Decided: done through the web interface (2), not in chat |
| 2 | [A web interface](#2-a-web-interface) | large | Everything is configured through environment variables and chat commands today; also where verification will live |
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

### Decision (2026-09-18): this is done in the web interface, not in chat

The chat-only version was thought through and dropped. It is technically possible -- the SAS
flow runs over Matrix itself, `matrix-sdk` 0.18 exposes all of it (`VerificationRequest`,
`SasVerification` with `emoji()`, `confirm()`, `mismatch()`, `cancel()`), and a bot can post
its seven emojis into the direct message and confirm its side. The problem is the other side
of the screen:

- **The emojis only exist once the popup is open.** They come out of the key exchange, and
  that is the moment the client shows them. The bot cannot post them ahead of time, and it has
  no channel other than the chat.
- **On mobile the popup covers the chat.** Element Web/Desktop shows the verification in a
  side panel and the timeline stays readable, so the comparison works there. Element
  Android/iOS put a sheet over the room, Element X takes the whole screen. A phone user cannot
  see what the bot posted, so the comparison -- the entire security property of SAS -- is
  impossible for them. What is left is pressing "they match" blind, which is trust-on-first-use
  dressed up as verification.
- The workarounds all amount to "use another screen": verify from the desktop once, read the
  bot's message on a second device, or compare the session fingerprint by hand under
  "Manually verify by text" (which Element X may not offer any more). None of them is something
  to send every user through.

A web page fixes exactly this: the bot's emojis, or a QR code, are shown on a screen the popup
is not covering. The user opens the page, starts the verification from their client, and
compares the client's emojis with the page -- or scans the QR code the page shows, which is the
flow every Element client already knows from verifying a new login. Nothing about the bot side
changes; it is the same `VerificationRequest` handling, with the page as the display.

So: verification is part of [2](#2-a-web-interface) and waits for it. Until then the
limitation stays as documented in the README.

### What to keep from the analysis

Things that were settled while thinking it through and hold for the web version too:

- **Every affected user verifies for themselves.** "Verified" is a judgement each client makes
  about the bot, not a state of the bot or the room. One person verifying does nothing for
  anyone else. Only people who have switched on "never send to unverified sessions" are
  affected at all; everybody else already shares keys with the bot and needs to do nothing --
  and stays that way whether or not this feature exists.
- **Anyone may verify, not only the admin.** The bot gains and gives away nothing by it; the
  user gains that the bot can read them. Restricting it to the admin would make it useless for
  everyone else.
- **What it actually costs today:** somebody with the strict setting on has their messages
  arrive at the bot as `m.room.encrypted`, so they are not counted for the weekly payout --
  and they are docked as idle despite being active. Reactions still count, those are not
  encrypted. That is the concrete reason to build it.
- **The bot can confirm its side automatically** once the user has confirmed theirs; the bot
  never uses its own trust in anybody's device, so the human comparison on the user's side is
  the whole check. The bot's confirmation is a formality the protocol requires.
- **A timeout.** A verification left half-finished should be cancelled (`sas.cancel()`),
  otherwise the next attempt runs into a flow that is still open.
- **Cross-signing.** Verifying the device against one person's session is the small version.
  The complete version is for the bot to have its own cross-signing identity
  (`bootstrap_cross_signing()`), so it is verified as a user rather than as a device and that
  survives a device change. The keys live in the crypto store under `STORE_PATH`; the gain
  only materialises with a recovery key exported somewhere and restored on a fresh device.
  Can be added later without changing anything the user does.
- **It does not fix the past.** Verification only affects keys shared from that moment on.
  Messages that were unreadable stay unreadable.

---

## 2. A web interface

Device verification (see [1](#1-device-verification)) is the first concrete reason to have
one: it needs a screen the client's verification popup does not cover, to show the bot's
emojis or a QR code on. Beyond that there are things chat commands are a poor fit for:
configuring the cooldown and the starting score per room, correcting a score by hand,
managing emojis with more comfort than `!register_emoji`, looking at a history, or seeing at a
glance which rooms the bot is even in.

The hard part is not the interface. It is the login.

### How people could log in

Two ways, neither of which puts a password anywhere near the bot.

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
the first place, and getting it out of that client and into the web interface is a manual
step.

#### b) OIDC through Matrix Authentication Service — the future-proof one, if you run MAS

[Matrix Authentication Service](https://github.com/matrix-org/matrix-authentication-service) is
a real OAuth 2.0 / OpenID Connect provider for Synapse, and the direction Matrix
authentication is moving in (MSC3861). Where it is deployed, a web interface can do an
ordinary authorization code flow against it and get back a verified Matrix user id — the same
"log in with …" you know from anywhere else, with a proper redirect and consent screen.

The condition is in the first sentence: the deployment has to run MAS. A plain Synapse with
local passwords does not have it. Worth designing for, not worth waiting for.

### Recommendation

(a) is the one that works against any Synapse today, so it is where to start. (b) once MAS is
common enough to assume — it is the same idea with a proper redirect instead of a manual step,
and both end at the same place: a verified Matrix user id, and no password anywhere near this
bot.

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
