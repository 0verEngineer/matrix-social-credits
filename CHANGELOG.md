# Changelog

All notable changes to this project are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.1.0] - 2026-09-04

### Fixed
- `!list` no longer shows users who left, were kicked or were banned. The listing is now
  filtered against the room's current member list; the stored scores are kept so they are not
  reset if somebody rejoins.
- 429 and other errors no longer take the bot down. A retry limit is configured on the client
  (which is also what makes the SDK retry plain connection failures at all), the login retries
  with backoff and respects `retry_after`, the sync loop survives transient errors, and no
  message send panics any more.
- Emojis are normalized identically when registered and when looked up. An emoji registered
  with a variation selector could previously never be matched again.
- The command handlers all returned `false` because of a stray `true;` statement, so none of
  the early returns in the dispatch worked.
- Editing a message no longer re-runs the command in it, and a message starting with `* ` is
  no longer parsed as a command.
- Users are identified by their Matrix id instead of a domain derived from
  `MATRIX_HOMESERVER_URL`. With `.well-known` delegation those differ, which meant the admin
  was never recognised and `!register_emoji` worked for nobody.
- Simultaneous reactions no longer lose score updates; the score is changed in a single SQL
  statement.
- The cooldown is measured from the oldest reaction in the window, not the newest, and no
  longer panics on a timestamp in the future.
- Autojoin works for rooms without a name (direct messages, freshly created rooms), stops
  retrying on permanent errors, and no longer logs success after giving up.
- Message bodies are HTML escaped and carry a real plaintext fallback. `!help` showed its
  `<emoji>` and `<social_credit>` placeholders as swallowed HTML tags.
- `Dockerfile` is buildable from a fresh clone again; it copies `Cargo.lock`, which was in
  `.gitignore`.

### Added
- `!unregister_emoji` to remove a registered emoji.
- Session and client state persistence, so a restart neither creates a new device nor triggers
  a full initial sync.
- Versioned database migrations, uniqueness constraints, indexes and WAL mode.
- Retention for the `event` deduplication table, which previously grew without bound.
- Structured logging through `tracing`, configurable with `RUST_LOG`.
- Graceful shutdown on `SIGTERM` and `Ctrl-C`.
- A test suite, and `fmt`, `clippy` and a multi-arch image build in CI.
- Images are built and published automatically for `linux/amd64` and `linux/arm64`: a release
  on a `v*` tag, `edge` on every push to `main`, and `pr-<n>` for every pull request. The
  workflow summary prints the pull command; `pr-*` tags are removed again when the pull
  request is closed.

### Changed
- matrix-sdk 0.6.2 → 0.18.0, Rust edition 2024, toolchain pinned in `rust-toolchain.toml`.
- Bot answers are `m.notice` instead of `m.text`.
- `ADMIN_USERNAME` also accepts a full Matrix id.
- Multi-stage container image on `debian:trixie-slim` running as a non-root user, roughly
  138 MB instead of about 1.5 GB.
- Version scheme: `0.0.9-alpha` → `0.1.0`. The `-alpha` suffix is gone; the leading `0.`
  already says that nothing here is stable. `Cargo.toml` is the single source of truth for
  the version, and CI refuses to publish an image whose tag disagrees with it.

### Removed
- `Dockerfile-arm`, which never worked (two `FROM` lines in the final stage, cross-compilation
  without a linker, EOL base image). Multi-arch is handled by `docker buildx` in CI.
- `Dockerfile-test`, which copied the whole working directory including `target/` and the
  local database into the image.
- The `regex` dependency.

## [0.0.8-alpha] - 2023
First alpha releases. See the git history.
