# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow the calendar: `<year>.<ISO week>.<n>`.

## [2026.45.0] - 2026-11-06

### Added

- Stateful, in-memory fake of the Forgejo REST API and git hosting for end-to-end tests that must not touch a real instance.
- API surface generated from Forgejo's pinned Swagger description; operations without an implementation answer `501 Not Implemented` with their `operationId`.
- Users, tokens, repositories, issues, labels and comments.
- Git over smart HTTP: repositories can be cloned and pushed to, and pushed content is visible through the API, including contents of empty repositories.
- PRs with review requests, live mergeability, and merging (including squash) onto the real git branches, titled as Forgejo titles them unless `MergeTitleField` says otherwise. Merging scheduled on checks stays pending until `Repository::advance`.
- Commit statuses, with the latest entry per context and Forgejo's priority ordering in combined reads.
- Branch comparisons based on git ancestry, including changed files.
- Repository webhooks, with a `TargetPolicy` that limits which addresses deliveries may contact.
- Actions runs and jobs, run listing and filtering, cancellation, scripted status frames, dispatch, job logs, and write-only Actions secrets.
- Fault injection (`fail_next`, `delay_next`), request profiling, and a request trace that omits authorization headers.
- HTTPS with `Options { tls: true, .. }` or `serve --tls`, using a throwaway local CA available from `ca_cert_path()`.
- `ninot-forgejo serve` command-line server that prints its URL and an admin token.
- Tested against the `tea` command-line client.
- Requires Rust 1.88 or newer (edition 2024).
