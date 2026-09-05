<div id="top"></div>


<!-- PROJECT SHIELDS -->
[![Contributors][contributors-shield]][contributors-url]
[![Forks][forks-shield]][forks-url]
[![Stargazers][stars-shield]][stars-url]
[![Issues][issues-shield]][issues-url]
[![GPLv3 License][license-shield]][license-url]


<!-- PROJECT LOGO -->
<br />
<div align="center">

<!-- description -->
Matrix bot for a social credit system
<!-- description end -->

  <p align="center">
    <br />
    <a href="https://codeberg.org/OverEngineer/matrix-social-credits">Codeberg</a>
    ·
    <a href="https://github.com/0verEngineer/matrix-social-credits">Github</a>
    .
    <a href="https://hub.docker.com/r/0verengineer/matrix-social-credits">Docker Hub</a>
    .
    <a href="https://github.com/0verEngineer/matrix-social-credits/issues">Report Bug</a>
    ·
    <a href="https://github.com/0verEngineer/matrix-social-credits/issues">Request Feature</a>
  </p>
</div>


---

<!-- TABLE OF CONTENTS -->
<details>
  <summary>Table of Contents</summary>
  <ol>
    <li>
      <a href="#about-the-project">About The Project</a>
    </li>
    <li>
      <a href="#setup">Setup</a>
    </li>
    <li><a href="#container-images">Container images</a></li>
    <li><a href="#configuration">Configuration</a></li>
    <li><a href="#commands">Commands</a></li>
    <li><a href="#operating-the-bot">Operating the bot</a></li>
    <li><a href="#limitations">Limitations</a></li>
    <li><a href="#development">Development</a></li>
    <li><a href="#license">License</a></li>
    <li><a href="#contact">Contact</a></li>
  </ol>
</details>


<!-- ABOUT THE PROJECT -->
## About The Project

- This is a Matrix bot for a social credit system.


<!-- SETUP -->
## Setup
- Use the example `docker-compose.yml` file to setup the bot.
- The bot user can be created with Element / Element Web or any other Matrix client that
  supports registering a new user.
- Invite the bot into a room; it accepts invitations automatically.
- The admin registers the emojis that change the score, see [Commands](#commands).


<!-- CONTAINER IMAGES -->
## Container images

Images are published to [Docker Hub](https://hub.docker.com/r/0verengineer/matrix-social-credits)
for `linux/amd64` and `linux/arm64`. The version comes from `Cargo.toml`; nothing is tagged by
hand any more.

| Tag | Points at | Use it for |
| --- | --- | --- |
| `latest` | the newest release | you want updates without touching the compose file |
| `0.1.0` | exactly that release | reproducible deployments, this is the recommended one |
| `0.1` | the newest patch release of that minor version | bug fixes only, no new behaviour |
| `0.2.0-rc.1` | a pre-release | testing a release candidate; never moves `latest` |
| `edge` | the current state of `main` | testing what is merged but not released |
| `pr-42` | the newest build of pull request 42 | reviewing or testing a pull request |

From `1.0.0` on there is also a bare major tag (`1`). While the project is still `0.x` that
tag is deliberately not published: a `0` that wanders across every `0.x` release would
promise a stability that does not exist yet.

`edge` and `pr-*` are development builds. They can contain half-finished work and, unlike a
release, are not guaranteed to have a working database migration path.

### Testing a pull request

Every pull request from this repository gets its own image. The workflow summary of the
`Publish` job prints the exact pull command, for example:

```sh
docker pull 0verengineer/matrix-social-credits:pr-42
```

There is also a `pr-42-<short sha>` tag that keeps pointing at one specific build, which is
useful when the branch is force-pushed while you are testing. Both tags are deleted again when
the pull request is closed.

Pull requests from a fork are built but not pushed: GitHub deliberately withholds the registry
credentials from them. Build such a branch locally instead:

```sh
docker build -t matrix-social-credits:test .
```

Or push it to a registry of your own, so a server can pull it:

```sh
docker buildx build -t your-registry/matrix-social-credits:test --push .
```

Add `--platform linux/amd64,linux/arm64` if you need both architectures; building for the
other one locally needs `binfmt`/QEMU (`docker run --privileged --rm tonistiigi/binfmt
--install all`) and is slow.

### Making a release

1. Bump `version` in `Cargo.toml` and run `cargo check` so `Cargo.lock` follows.
2. Update `CHANGELOG.md`.
3. Merge that into `main`.
4. Tag the merge commit and push the tag:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

The tag only triggers the release; the image tags are derived from `Cargo.toml`. If the two
disagree, the workflow fails instead of publishing a mislabelled image. A version with a
pre-release suffix (`0.2.0-rc.1`) is published under that exact tag only and moves neither
`latest` nor `0.2`.

### Repository secrets

| Name | Kind | Needed for |
| --- | --- | --- |
| `DOCKERHUB_USERNAME` | **variable** | pushing any image |
| `DOCKERHUB_TOKEN` | secret | pushing any image; needs the *Read, Write, Delete* scope so closed pull request tags can be cleaned up again |
| `CODEBERG_TOKEN` | secret | the Codeberg mirror |

`DOCKERHUB_USERNAME` has to be a **variable**, not a secret, and the workflow stops with an
explicit error if it finds a secret of that name. The user name is the first half of the
public image name, so nothing is gained by hiding it -- but GitHub masks secret values
everywhere, and it drops a job output that contains one instead of passing it on. The tag list
would arrive empty at the publish job, and every log line would read
`***/matrix-social-credits`.

Without them the workflow still builds both architectures and says in the job summary which
one is missing. Note that a run only sees the secrets and variables that existed when it
started -- adding one does not fix a run that is already going, you need a new run.


<!-- CONFIGURATION -->
## Configuration

### Required environment variables
| Variable | Description |
| --- | --- |
| `MATRIX_HOMESERVER_URL` | Homeserver URL of the bot user, for example `https://matrix.org`. Must include the scheme. |
| `MATRIX_USERNAME` | Localpart of the bot user, for example `social-credit-system`. |
| `MATRIX_PASSWORD` | Password of the bot user. Only used for the very first login, see [Sessions](#sessions). |
| `ADMIN_USERNAME` | The user allowed to register emojis. Either a bare localpart (`alice`) or a full Matrix id (`@alice:example.org`). |
| `INITIAL_SOCIAL_CREDIT` | Score a user starts with in a room. |
| `REACTION_LIMIT` | How many score changing reactions a user may make within `REACTION_TIMESPAN`. |
| `REACTION_TIMESPAN` | Length of that window, in minutes. |
| `DB_PATH` | Path to the SQLite database file. |

A bare `ADMIN_USERNAME` is resolved against the **server name of the bot's own Matrix id**,
which the homeserver reports after login. That is not necessarily the host in
`MATRIX_HOMESERVER_URL`: with `.well-known` delegation the URL can be
`https://matrix.example.org` while user ids read `@alice:example.org`. Give the full Matrix id
if you are unsure.

### Optional environment variables
| Variable | Default | Description |
| --- | --- | --- |
| `STORE_PATH` | `store` next to `DB_PATH` | Directory for the client state store and the saved session. |
| `RUST_LOG` | `matrix_social_credits=info,matrix_sdk=warn` | Log filter, see [Logging](#logging). |
| `HTTP_RETRY_LIMIT` | `10` | How often a single HTTP request is retried. |
| `HTTP_MAX_RETRY_TIME_SECS` | `60` | Upper bound for the wait between two attempts of the same request. |
| `LOGIN_RETRY_BUDGET_SECS` | `900` | How long the initial login keeps retrying before the bot gives up and exits. |
| `EVENT_RETENTION_DAYS` | `30` | How long the deduplication markers in the `event` table are kept. |


<!-- COMMANDS -->
## Commands
| Command | Who | Description |
| --- | --- | --- |
| `!help` | everyone | Show the command list. |
| `!list` | everyone | Social credit scores of everyone currently in the room. |
| `!list_emoji` | everyone | Registered emojis and their score change. |
| `!register_emoji <emoji> <score>` | admin | Register an emoji, e.g. `!register_emoji 😑 -25`. |
| `!unregister_emoji <emoji>` | admin | Remove a registered emoji again. |

`-` and `_` are interchangeable in every command, and `!list_emoji`, `!list-emoji`,
`!list_emojis` and `!list-emojis` all work.

### Usage
React with a registered emoji to a message to change the score of the user who sent it.

- You cannot change your own score.
- Each message counts once per user; reacting a second time to the same message does nothing.
- Variation selectors and skin tone modifiers are ignored, so 👍 and 👍🏽 are the same emoji as
  far as the bot is concerned.


<!-- OPERATING -->
## Operating the bot

### Sessions
After the first successful login the session is written to `STORE_PATH/session.json` with
`0600` permissions and reused on every following start. This matters for two reasons: a fresh
login creates a new device each time, and `/login` is one of the endpoints Synapse rate limits
hardest. The password is only needed again if the session is revoked.

The same directory holds the client state store, including the sync token, so a restart
resumes where the previous run stopped instead of replaying the timeline.

Back up `STORE_PATH` together with the database, or the bot logs in again and re-syncs.

### Encrypted rooms
The bot works in encrypted rooms. `STORE_PATH` also holds its crypto store, so the device and
its room keys survive a restart -- which is what keeps the bot able to read messages sent
while it was down, once it catches up.

Two consequences worth knowing:

- Delete `STORE_PATH` and the bot loses its device identity along with every room key it had.
  It logs in again as a new device and can only read messages sent from that point on. The old
  devices stay on the account until somebody removes them in a client.
- The bot is an unverified session. That is fine by default, but see
  [Limitations](#limitations) if members of your room restrict encryption to verified
  sessions.

### Rate limits and restarts
Synapse answers with `429 M_LIMIT_EXCEEDED` fairly often, especially while the Matrix stack is
coming back up. The bot handles this in three places:

- Requests are retried according to `HTTP_RETRY_LIMIT` and `HTTP_MAX_RETRY_TIME_SECS`, and a
  `retry_after` sent by the server is respected. Setting a retry limit is also what makes the
  SDK retry plain connection failures at all, which is the case while the homeserver is down.
- The login retries within `LOGIN_RETRY_BUDGET_SECS`. Permanent errors such as a wrong
  password fail immediately instead of looping.
- The sync loop survives transient errors. Only a permanent failure ends it.

A single request can therefore take several minutes before it gives up. Run with
`RUST_LOG=matrix_sdk=debug` to see the individual attempts.

### Logging
Logging goes to stdout via `tracing`. `RUST_LOG` takes the usual filter syntax:

```
RUST_LOG=matrix_social_credits=debug          # more detail from the bot
RUST_LOG=matrix_social_credits=trace          # includes full event payloads
RUST_LOG=matrix_social_credits=info,matrix_sdk=debug   # SDK request and retry detail
```

`trace` logs message contents. Do not leave it on in production.

### Database
SQLite, at `DB_PATH`, in WAL mode -- back up `*.db`, `*.db-wal` and `*.db-shm` together, or
stop the bot first.

The schema is versioned through `PRAGMA user_version` and migrated on start. Migrations run
automatically and are idempotent, but take a backup before upgrading anyway.

Scores of users who left a room are kept, so they are not reset if somebody rejoins. They are
only hidden from `!list`.

### Shutdown
The bot handles `SIGTERM` and `Ctrl-C`, so `docker stop` shuts it down cleanly.


<!-- LIMITATIONS -->
## Limitations
- **The bot's device is never verified.** It works in encrypted rooms, but it shows up as an
  unverified session. Anybody who has "never send encrypted messages to unverified sessions"
  switched on will not share room keys with it, so the bot cannot read *their* messages.
- **Only messages sent after the bot's device existed can be read.** The bot cannot decrypt
  anything from before it joined, and deleting `STORE_PATH` throws its identity away and
  starts a new device.
- There is exactly one admin, configured through `ADMIN_USERNAME`. There is no command to
  promote anybody.
- Removing a reaction does not undo the score change.
- `REACTION_LIMIT`, `REACTION_TIMESPAN` and `INITIAL_SOCIAL_CREDIT` are global, not per room.
- `!list` shows at most 100 users.


<!-- DEVELOPMENT -->
## Development
The toolchain is pinned in `rust-toolchain.toml`; `rustup` picks it up automatically.

```
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

Building the container image:

```
docker build -t matrix-social-credits .
docker buildx build --platform linux/amd64,linux/arm64 -t matrix-social-credits .
```


<!-- LICENSE -->
## License

Distributed under the GNU General Public License v3 See `LICENSE` for more information.



<!-- CONTACT -->
## Contact

Julian Hackinger - dev@hackinger.net

Project Link: [https://github.com/0verEngineer/matrix-social-credits](https://github.com/0verEngineer/matrix-social-credits)



<!-- MARKDOWN LINKS & IMAGES -->
[contributors-shield]: https://img.shields.io/github/contributors/0verEngineer/matrix-social-credits.svg?style=for-the-badge
[contributors-url]: https://github.com/0verEngineer/matrix-social-credits/graphs/contributors
[forks-shield]: https://img.shields.io/github/forks/0verEngineer/matrix-social-credits.svg?style=for-the-badge
[forks-url]: https://github.com/0verEngineer/matrix-social-credits/network/members
[stars-shield]: https://img.shields.io/github/stars/0verEngineer/matrix-social-credits.svg?style=for-the-badge
[stars-url]: https://github.com/0verEngineer/matrix-social-credits/stargazers
[issues-shield]: https://img.shields.io/github/issues/0verEngineer/matrix-social-credits.svg?style=for-the-badge
[issues-url]: https://github.com/0verEngineer/matrix-social-credits/issues
[license-shield]: https://img.shields.io/github/license/0verEngineer/matrix-social-credits.svg?style=for-the-badge
[license-url]: https://github.com/0verEngineer/matrix-social-credits/blob/main/LICENSE
