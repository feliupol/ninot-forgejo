# ninot-forgejo

A stateful, in-memory fake of the Forgejo REST API and git hosting, for
end-to-end tests that must not touch a real forge.

Like its GitHub counterpart, it keeps state: repositories created through the API
can be cloned and pushed to over smart HTTP, and what is pushed is visible through
the API. Its API surface is generated from Forgejo's Swagger description, pinned in
`spec/` with `scripts/update-spec.sh`. Operations without an implementation answer
`501 Not Implemented` with their `operationId`.

## Build

Build and test with `cargo build` and `cargo test`.

## Use

In Rust tests:

```rust
let server = ninot_forgejo::Server::start()?;
let alice = server.create_user("alice");
let token = alice.create_token(&["write:repository"])?;
// API at server.url() + "/api/v1", git at server.url()/{owner}/{repo}.git
```

- `Server.create_user` and `User.create_repository` return handles for those resources;
  `Server.user` and `Server.repository` look up resources created through HTTP.
- `Repository.commit_file` makes a real Git commit and advances the branch. A missing branch
  starts from the default branch. Its optional `executable` flag sets or clears the
  Git executable bit; omitting it preserves Git's file-mode behavior.
  `Repository.create_branch` can choose a different starting ref.
- Status writes append history. Combined status reads keep the latest entry per
  context and apply Forgejo's priority ordering.
- Branch comparisons use Git ancestry and include changed files. PR lists filter
  by base and head. Merges update the actual Git branches, including squash merges. Without `MergeTitleField` a merge commit is titled
  `Merge pull request '<title>' (#<index>) from <head> into <base>` and a squash
  `<title> (#<index>)`, as Forgejo does.
- `Server(tls=True)` (Rust `Options { tls: true, .. }`, CLI `serve --tls`) serves
  HTTPS from a throwaway local CA; `Server.ca_cert_path` (Rust
  `ca_cert_path()`) is the PEM clients trust, for example through `SSL_CERT_FILE`
  or git's `http.sslCAInfo`.
- `Repository.create_pull_request`, `Repository.create_label`,
  `PullRequest.request_review` and `Repository.create_action_task` seed the
  resources used by repository search, review queues and CI collection.
- `Repository.create_action_run(run, jobs)` seeds a workflow run and its ordered
  jobs and returns an `ActionRun` handle. The public Actions run APIs list/filter/page runs, read their jobs and
  cancel unfinished jobs without changing completed predecessors.
- `ActionRun.script_status_frames(frames)` provides explicit successive
  job-status frames. Each run-list or individual-run observation advances one
  frame and persists the aggregate status, so controllers can observe a run
  finishing without sleeps or another HTTP fixture service. Each frame must
  name every job.
- `Repository.script_workflow_dispatch(workflow, ref, inputs, run)` maps a
  real workflow-dispatch request to a run already seeded with
  `Repository.create_action_run`.
  It validates the workflow, ref, and inputs and returns the run identifier when
  `return_run_info` is requested; the seeded run's status frames remain the
  only source of progress. Use `commit_file` to seed any branch a completed job
  would leave behind instead of teaching the fake an application-specific job.
- `Repository.create_action_secret` and `Repository.action_secret_names` seed or inspect Actions secrets
  by name. HTTP PUT/DELETE and list requests use the normal Forgejo protocol;
  secret values are write-only and are never retained in fake state.
- `start_request_trace()` and `stop_request_trace()` opt into a direct fixture
  capture of every REST request attempt, including rejected and injected-fault
  requests, with method, path, query, and body bytes. The trace omits
  authorization headers, retains at most 8 MiB per capture, and returns a
  `truncated` flag so a security assertion can fail closed if capture was
  incomplete. Body bytes are copied only while tracing is enabled.
- `PullRequest.scheduled_merge()` returns named `merge_method` and `delete_branch`
  fields when a merge is pending. A merge waiting for reported checks stays
  pending until `Repository.advance()`.
  It completes only after the current head has successful or skipped checks for
  every context that was required when the merge was scheduled.
- `fail_next(method, path, status)` and `delay_next(method, path, milliseconds)`
  inject single-use failures. Paths include `/api/v1`. `start_profile()` and
  `stop_profile()` observe requests actually received by the service.
- Fixture methods are direct APIs. They do not add HTTP routes to the fake service.

## CLI conformance

`cargo test` runs the official Forgejo CLI, [`tea`], against the fake, one
test per check (`whoami`, repositories, issues, labels, `git` clone and push,
pull requests). The CLI binary is taken from `$TEA`, or `tea` from `$PATH`.

[`tea`]: https://gitea.com/gitea/tea

## Status

Early development.

## License

MIT
