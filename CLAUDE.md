# Rust — guidelines

## Philosophy

Write code like a senior developer who knows this repository.
Code should carry decisions, not obvious restatements. If a comment
repeats the function signature, delete it. If a `.clone()` could be
a reference, use the reference. If a trait bound isn't exercised in
the body, remove it. Sterile perfection is the strongest signal
that an AI wrote it.

## Code rules

- Do not include any game-specific or shader-specific hacks. Fixes must address the underlying emulation behavior, not special-case individual titles or individual shaders.
- Do not include tests. If emulation behaviour is changed, write and share hardware tests in the form of OpenOrbis homebrew.
- Do not comment code unless absolutely necessary, and never comment code that is already self-explanatory. Express intent through clear naming and structure instead.
- Maintain code conventions existing elsewhere in the project. Match the surrounding code's naming, formatting, file layout, error handling, and use of existing helpers and utilities. Follow the repository's `.clang-format` configuration.

## Git workflow

- Never merge main into local feature branches, always rebase.

## General expectations

- Keep changes focused and minimal. Do not refactor or reformat unrelated code.
- Prefer reusing existing abstractions in the codebase over introducing new ones.
- Make sure the project builds before proposing changes.

## Code

- Comments in English, explain WHY (a constraint, a bug fix, an
  invariant, a workaround, a causal chain), never WHAT the code
  already shows. A comment is noise if deleting it loses no
  information that isn't in the signature. Keep `// caches results
  for 5 minutes because upstream poll interval changed (see #42)`;
  drop `// Process each item`. Allowed human markers: `// SAFETY:`,
  `// TODO`, `// FIXME`, `// NOTE`, `// HACK`, `// XXX`, `// WARN`,
  plus any sentence with a causal keyword: `because`, `since`,
  `so that`, `otherwise`, `to avoid`, `to prevent`, `to ensure`,
  `until`, `once`, `due to`. If your comment has one of those
  connectors, it's explaining WHY — keep it.
  Caught by: `obvious-comment`.
- Doc comments (`///`) document contracts and invariants for callers.
  `# Arguments` / `# Returns` sections that restate types are noise.
  A good doc comment answers "why does this exist?" or
  "what contract must the caller uphold?", not "what does this return?".
  `# Panics`, `# Errors`, `# Safety`, and `# Examples` sections are
  always welcome. If at least one argument carries contractual info
  (`must`, `ownership`, `borrowed`, `valid`, `non-empty`), the section
  survives.
  Caught by: `doc-restates-signature`.
- Validate at the edge (`validate.rs` per crate): repo path, rev,
  sha, tree path. Network input ends up as an FS path, an S3 key
  and a git argument.
- External arguments to git go after `--end-of-options`
  (upload-pack/receive-pack excepted — they don't take it).
- `unsafe_code = forbid`, clippy `all = warn`, no bypasses.
  `cargo fmt` before committing (`max_width = 120`).
- No speculative abstraction: fix what was asked, nothing alongside
  it. No oversized files — a module outgrowing its siblings becomes
  a directory module. Custom macros only after a pattern repeats
  3+ times in this codebase, with a project-prefixed name.
- Type inference: don't annotate types or lifetimes the compiler
  can infer. Explicit only on public APIs or genuine ambiguity.
  Caught by: `trivial-type-annotation`, `explicit-lifetime-elision`.
- Reference > move > clone. Before `.clone()`, ask: can I borrow?
  Can I move? Clone only when ownership demands it.
  Caught by: `possible-borrow-instead-of-clone`, `redundant_clone`,
  `clone_on_ref_ptr`.
- Minimal trait bounds: if `Hash` isn't used in the body, it
  doesn't belong in the `where` clause. Remove what the compiler
  doesn't ask for. Caught by: `extra_unused_type_parameters`,
  `trait_duplication_in_bounds`.
- Selective derives: `#[derive(...)]` only what you need. Think
  before stacking. Manual `PartialEq` if the comparison should
  ignore fields. More than 4 derives triggers a review.
  Caught by: `excessive-derives`, `derivable_impls`.
- Errors with semantics: don't propagate `?` blindly. Map errors
  by business context. `NotFound` is not `PermissionDenied`.
  No `Err(_)` catch-all on closed enums without justification.
  Caught by: `wildcard-error-match`, `match_wild_err_arm`.
- Concurrency by case: `AtomicU32` for counters, `RwLock` if
  reads >> writes, channels for producer-consumer, `Weak` for
  parent links in `Arc` graphs. `Mutex` is the last resort.
  Caught by: `mutex_atomic`, `mutex_integer`, `mutex-on-primitive`.
- No intermediate `collect()`: chain iterators to the destination.
  If you need a `Vec`, pre-allocate with `with_capacity`.
  Caught by: `needless_collect`, `unnecessary_collect`.
- Async without blockers: no `block_on()` inside async contexts.
  Within the async runtime, everything is async. Block synchronously
  outside. Caught by: `block-on-in-async`.
- Structured logging with `tracing`: named fields, target per
  module. Never `println!` or `log::info!("Processing started")`
  in production code. Caught by: `println-in-production`.

## Commits

- One logical change each. Message is one line, imperative, no body.
  Doesn't fit? Too big — split it.
- Several features in one request: commit each as it lands, never
  batched at the end.
- Never `Co-Authored-By: Claude` or any AI mention.
  Caught by: `ai-attribution` (scans commit messages).

## Versioning

SemVer as it reads below 1.0 (§4 plus Cargo's rule: in `0.y.z`
the breaking slot is `y`). Derive the number from the commits in
the range, never by feel:

| Commits in the range | Bump |
|---|---|
| anything breaking (`feat!:`, `fix!:`, `BREAKING CHANGE:` footer) | `0.(y+1).0` |
| `feat`, `fix`, `perf` | `0.y.(z+1)` |
| only `docs`/`chore`/`style`/`refactor`/`test`/`ci`/`build` | no release |

- A breaking change says so in the commit, or the number can't be
  derived without reading the diff.
- What breaks differs per repo: `gitquarry` is a server (CLI flags,
  `gitquarry.toml` keys, ssh/HTTP routes, bucket layout); `s3db`
  is a library (exported API — `cargo semver-checks` answers it).
- Tag notes are a technical record: terse bullets anchored in code
  (module, function, route, storage prefix, constant and value),
  facts read from the tree *at that tag*, no narrative.
- One version per workspace (`[workspace.package]`), never a crate
  at a time.
- Write `tag-changelog/vX.Y.Z.md` first — `webui::changelog`
  `include_str!`s it by `CARGO_PKG_VERSION`, so a version without
  notes doesn't compile — then the bump as its own commit, then
  tag `vX.Y.Z` annotated and signed on `master`, after the
  `develop` -> `master` PR.
- `CHANGELOG.md` is the other half of the notes, grouped by area.
  An `s3db` repin alone is a `chore` and earns no version.

## Security

- Nothing destructive (`rm`, S3 deletes, `git reset --hard`) without
  confirming first.
- Changes to `.gitignore`, `.conf/` or the passphrase: warn about
  the impact, don't assume.

## Automation reference

| Pattern | Enforcement layer |
|---|---|
| Obvious comments | `check-humanization.sh` → `obvious-comment` |
| Doc restates signature | `check-humanization.sh` → `doc-restates-signature` |
| Trivial type annotations | `check-humanization.sh` → `trivial-type-annotation` |
| Explicit elidable lifetimes | `check-humanization.sh` → `explicit-lifetime-elision` |
| Clone where borrow suffices | Clippy + `check-humanization.sh` |
| Unused trait bounds | Clippy → `extra_unused_type_parameters` |
| Excessive derives (> 4) | `check-humanization.sh` → `excessive-derives` |
| Wildcard error match | Clippy + `check-humanization.sh` |
| Mutex on primitives | Clippy + `check-humanization.sh` |
| Intermediate `collect()` | Clippy → `needless_collect` |
| `block_on()` in async | `check-humanization.sh` → `block-on-in-async` |
| `println!` in production | `check-humanization.sh` → `println-in-production` |
| AI attribution in commits | `check-humanization.sh` → `ai-attribution` |
| Format violations | `cargo fmt --check` |
| Semver breaks (libraries) | `cargo semver-checks` |