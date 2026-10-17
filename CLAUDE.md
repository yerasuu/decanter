# Rust — guidelines

## Philosophy

Write code like a senior developer who knows this repository.
Code should carry decisions, not obvious restatements. If a comment
repeats the function signature, delete it. If a `.clone()` could be
a reference, use the reference. If a trait bound isn't exercised in
the body, remove it. Sterile perfection is the strongest signal
that an AI wrote it.

## Code rules

- Do not include any game-specific or shader-specific hacks in the core. Fixes must address the underlying emulation behavior, not special-case individual titles or individual shaders. The one exception is psport's temporary per-title fixes (see the psport section).
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
- Doc comments (`///`) document contracts and invariants for callers.
  `# Arguments` / `# Returns` sections that restate types are noise.
  A good doc comment answers "why does this exist?" or
  "what contract must the caller uphold?", not "what does this return?".
  `# Panics`, `# Errors`, `# Safety`, and `# Examples` sections are
  always welcome. If at least one argument carries contractual info
  (`must`, `ownership`, `borrowed`, `valid`, `non-empty`), the section
  survives.
- Validate at the edge, once per component: GOG API answers, files
  read from games and PS dumps (`param.sfo`, manifests), the port's
  repository and release data, and paths the user picks are checked
  where they enter, before they become a filesystem path, a process
  argument or a database row.
- External arguments to git and other tools go after
  `--end-of-options` (or `--` where the tool has no such option).
- `unsafe_code = forbid`; clippy `all`, `pedantic`, `restriction`
  and `nursery` at warn with the `deny` list in the workspace
  `Cargo.toml`, no bypasses. `cargo fmt` before committing
  (`max_width = 120`).
- No speculative abstraction: fix what was asked, nothing alongside
  it. No oversized files — a module outgrowing its siblings becomes
  a directory module. Custom macros only after a pattern repeats
  3+ times in this codebase, with a project-prefixed name.
- Type inference: don't annotate types or lifetimes the compiler
  can infer. Explicit only on public APIs or genuine ambiguity.
- Reference > move > clone. Before `.clone()`, ask: can I borrow?
  Can I move? Clone only when ownership demands it.
  Clippy: `redundant_clone`, `clone_on_ref_ptr`.
- Minimal trait bounds: if `Hash` isn't used in the body, it
  doesn't belong in the `where` clause. Remove what the compiler
  doesn't ask for. Clippy: `extra_unused_type_parameters`,
  `trait_duplication_in_bounds`.
- Selective derives: `#[derive(...)]` only what you need. Think
  before stacking. Manual `PartialEq` if the comparison should
  ignore fields. More than 4 derives triggers a review.
  Clippy: `derivable_impls`.
- Errors with semantics: don't propagate `?` blindly. Map errors
  by business context. `NotFound` is not `PermissionDenied`.
  No `Err(_)` catch-all on closed enums without justification.
  Clippy: `match_wild_err_arm`.
- Concurrency by case: `AtomicU32` for counters, `RwLock` if
  reads >> writes, channels for producer-consumer, `Weak` for
  parent links in `Arc` graphs. `Mutex` is the last resort.
  Clippy: `mutex_atomic`, `mutex_integer`.
- No intermediate `collect()`: chain iterators to the destination.
  If you need a `Vec`, pre-allocate with `with_capacity`.
  Clippy: `needless_collect`.
- Async without blockers: no `block_on()` inside async contexts.
  Within the async runtime, everything is async. Block synchronously
  outside.
- Structured logging with `tracing`: named fields, target per
  module. Never `println!` or `log::info!("Processing started")`
  in production code.

## C / C++ (psport)

decanter builds and runs psport (`crates/ps`); work on psport's C and C++ follows psport's own
`CLAUDE.md`, which wins over these Rust guidelines there:

- Everything native: each PS4 system library is implemented in the port (HLE). A dumped system
  module may be used to test, to detect behaviour or as a temporary patch, never as the solution.
- How the firmware or a game really does something is read from the original code with the `rea`
  subagent (REA over Ghidra; its MCP server runs only while the subagent does), in any branch or
  worktree; its findings become kb notes or tasks.
- Optimisations that only help one game go in that game's profile (`gpu/games/<game>/`); the core
  runs every game correctly without one.
- A temporary per-title correctness fix is allowed only as a declared WORKAROUND quirk (decision
  #331): its code lives in that game's section (`gpu/games/<game>/`), never in the core; it is
  logged, disableable with `PS_QUIRKS`, references a kb task that seeks the general fix, and is
  removed when that fix lands.
- Nothing silent: unimplemented, stubbed or failing calls stay visible with counts; logs in
  technical English.
- The GPU path is cross-vendor Vulkan (AMD, NVIDIA, Intel; decision #361): chosen by reported
  capability, never by vendor id; vendor or driver checks only as `HOST` quirks for driver defects.
  Whatever cannot be done the same way on all three is recorded first (kb note tagged `gpu` and
  `vendor`) so the user decides whether a vendor-specific part is worth it.
- Role and scope prefixes in all of psport's own code (not the inherited `gpu/shadps4` tree,
  except the lines psport adds, and not SDK/ABI names), in `snake_case` as the surrounding code:
  `cb_` bytes, `c_` count, `i_` index, `off_` offset, `va_` integer address, `h_` handle, `sz_`
  string, `p_`/`pp_` pointers, `pfn_` function pointers; booleans as predicates (`is_mapped`);
  scope first (`g_`, `s_`, `m_`); `out_` for output parameters. No type-width prefixes. The full
  table is in psport's `CLAUDE.md`. Existing code is converted area by area in its own commits,
  never mixed into a fix.

## Commits and branches

- Branches: `main` (what users get, a tag per version), `develop` (integration),
  `<type>/<kb id>-<topic>` per task. Into `develop` by rebase, squash and fast-forward (linear
  history); into `main` by fast-forward once validated. The user pushes to GitHub (`origin`);
  never rewrite what is published unless the user orders it. See docs/git-workflow.md
  (decision #376).
- Once a branch is integrated and its task finished, clean it up as part of the integration: remove
  its worktree (`git worktree remove`) and delete the branch (`git branch -d`), after checking its
  content is in `develop`. Never leave integrated branches or worktrees behind.
- One useful commit per task or feature on `develop`, with everything the change carries (docs,
  comments, locale strings); split only when each part stands on its own (e.g. a large mechanical
  move apart from the behaviour change). Branch checkpoints are squashed at integration.
- Always Conventional Commits: `type(area): subject`, imperative English, ~50 characters and 72 at
  most; type and area from the board tags. Optional body of one to four lines, after a blank line,
  with the why when it is not obvious (cause, evidence, measured effect); `BREAKING CHANGE:`
  footer when it breaks. Signed.
- Never `Co-Authored-By: Claude` or any AI mention.
  Caught by the `.githooks/commit-msg` hook (`git config core.hooksPath .githooks`).

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
- decanter is an application: what breaks is its stored data and
  layout (the library's `decanter/` folder, the database schema,
  `libraries.toml`), its CLI and its settings.
- Tag notes are a technical record: terse bullets anchored in code
  (module, function, setting, path, constant and value), facts read
  from the tree *at that tag*, no narrative.
- One version per workspace (`[workspace.package]`), never a crate
  at a time.
- Write the version's notes first (`tag-changelog/vX.Y.Z.md`, with
  the build failing without it), then the bump as its own commit,
  then tag `vX.Y.Z` annotated and signed on `main` once `develop`
  has fast-forwarded into it.
- `CHANGELOG.md` is the other half of the notes, grouped by area.
  A dependency repin alone is a `chore` and earns no version.

## Security

- Nothing destructive (`rm`, deleting user data, `git reset --hard`)
  without confirming first.
- Changes to `.gitignore` or to where secrets and credentials live
  (keyring, GOG tokens): warn about the impact, don't assume.

## Board tags

Every `kb` entry carries tags from this fixed set; add a new one only when
no existing tag fits, and record it here.

- Area, one or two per entry:
  - decanter: `app` (GPUI views and widgets), `cli`, `gog` (auth, library,
    installs, cloud saves), `achievements` (Galaxy hook, achievement cache,
    trophies), `runtime` (Proton/umu-run launch), `library` (roots, profiles,
    storage, external games), `desktop` (tray, taskbar, desktop entries),
    `i18n`, `ps` (decanter's PS section: port builds, settings, runs).
  - psport: `psport` (structure, launch, settings, quirks), `loader` (SELF/ELF,
    linking, patches, mods), `kernel`, `system` (services, savedata, trophies,
    NP offline, dialogs), `gpu` (translator, shaders, Vulkan, upscalers),
    `audio`, `video`, `input`, `net` (online, sockets, HTTP).
  - cross-cutting: `vendor` (a GPU vendor difference waiting for the user's decision on a
    vendor-specific part), `regression` (something that worked and stopped working, whatever
    its area; it goes with that area's tag).
  - shared: `toolchain` (Nix, CI, prebuilt ports), `regress-test` (the regress tests:
    tools/regress, schedules, perf runs, night test sessions), `workflow` (publishing flow,
    agent process and environment), `brain` (the board itself).
- State: `testing` on a task whose code is written but not yet checked running (a game, the
  app, a real CI release); it stays parked with that tag until the check, then `done`.
- Platform, only when the entry concerns some systems or architectures and not
  the rest (none means all, Linux x86_64 being the reference): `linux`,
  `windows`, `macos`; `x86_64`, `aarch64`.
- Title, when the entry is about one game: `bloodborne`, `ryza`, `ayesha`,
  `symphonia`, `saga`, `astrea`, `altdeus`, `onigiri`, `galguardians`,
  `galguardians2`, `bo1`, `bo2`, `bloodomen2`, `octopath0`.
- Type, on every task: the Conventional Commits type its commits carry
  (or will) — `feat`, `fix`, `perf`, `refactor`, `style`, `docs`, `test`,
  `build`, `ci`, `chore`. A task whose work lands as two types takes both.
  Board-only work (seeding, reviews, investigations without code, night
  test runs) is `chore`. Notes and decisions take none: they aren't commits.
  `kb list --tag fix` is what the next version's notes draw from.
- Security entries add standard tags: `cwe-N` (MITRE CWE id, one or more),
  `owasp-aNN` (OWASP Top 10:2025 category, only where it clearly fits),
  `sev-high` / `sev-medium` / `sev-low` (on findings only, never on design
  decisions). Process notes take no CWE.
- Board text is in Spain Spanish; tags, identifiers and code stay verbatim.

## Automation reference

| Pattern | Enforcement layer |
|---|---|
| Clone where borrow suffices | Clippy → `redundant_clone`, `clone_on_ref_ptr` |
| Unused trait bounds | Clippy → `extra_unused_type_parameters` |
| Derivable impls | Clippy → `derivable_impls` |
| Wildcard error match | Clippy → `match_wild_err_arm` |
| Mutex on primitives | Clippy → `mutex_atomic`, `mutex_integer` |
| Intermediate `collect()` | Clippy → `needless_collect` |
| Commit format and AI attribution | `.githooks/commit-msg` |
| Format violations | `cargo fmt --check` |

The comment, doc-comment, type-annotation, derive-count, `block_on` and `println!` rules above are
review rules: nothing checks them automatically.

## Recording work (mandatory)

Everything done in a session is recorded on the board as it happens, in decanter and in psport
alike: the user follows it live and it is the only record other sessions read. This is not
optional and not a summary at the end.

- Work that changes a repository, the user's files or settings gets its task *before* it starts
  (`kb add --start`), also quick follow-ups after a question.
- Each step lands as a comment when it happens: builds, test runs and their results, rebases and
  how each conflict was resolved, integrations, cleanups, changes to the user's settings.
- Every answer the user gives (in chat or to a question) is a decision: `kb add --decision`.
  Every non-obvious finding is a note: `kb add --note`. Pasted logs analysed go on their tasks.
- Before ending a turn, check that nothing done or decided in it is missing from the board.

<!-- kanbrain:start -->
## Project memory: kanbrain

This project's tasks, to-dos, decisions and notes live in `.brain/`, managed only with `kb`, kanbrain's CLI.
The user follows them live on a kanban board: keep it showing what you're doing as you go, not at the end.

- Start of session: `kb context`, unless its output is already above (a session hook adds it).
- Work that changes the repository, or that the user should see, gets a task; questions and lookups don't. Find it
  (`search WORDS`) or create it (`add TITLE`), then `start ID` (`add --start TITLE` does both). "Todos", a plan or
  what's pending, in any language, are tasks here, one `add` each; your built-in to-do list isn't on the board, so
  keep it for steps within the task you've started. A task with real work gets a body, `add --body - TITLE`:
  **Goal** (what and why, a line or two) and **Done when** (what the user will check).
- While working: `comment ID TEXT` when a step is done, when you find a cause, and before you stop (one or two
  lines: what changed, what's next); `done ID` as soon as one is finished. Stopping before it is: `comment`, then
  `park ID`. Doing isn't limited for you: several of your tasks can run at once. Never park, finish or move a task
  you didn't start (the user's, or another session's).
- Splitting work between subagents: give each part that changes the repository its own task, and run the board
  yourself: `add --start` it before launching the subagent, then `comment` its result and `done` it. Subagents
  only do the work; lookups and research get no task. `kb` commands can run in parallel.
- Blocked on the user (a requirement, access, a choice that's theirs): `ask ID TEXT` parks the task with the
  question; don't guess. A choice you shouldn't make alone and weren't asked to (architecture, a dependency,
  storage, a public interface): `propose ID --body - TITLE`, Context / Options / Recommendation on stdin; it parks
  the task until the user accepts or rejects it. Then, unless the user said they're away, if your tool can run a
  command in the background and wake you when it exits, run `kb wait ID` (the task) that way and end your turn: it
  prints the answer, or that there's none yet (then stop, don't wait again). Otherwise go on with other work or
  stop; answers come first in `context`, under Answered: `show ID`, then pick the task up. If the user answers in
  the chat instead, `start` the task (that ends the wait). `accept`/`reject` a decision only when the user tells
  you to.
- Choosing between approaches within your task: `search` for earlier decisions (only accepted ones are in force),
  then `add --decision --link ID --body - TITLE` with Context / Decision / Consequences on stdin. Non-obvious
  findings: `add --note --link ID --body - TITLE`; a problem someone has to fix is a task instead, in the
  repository that has to change.
- `## Project` in `context` is the application's shared context. If it isn't described, or you're asked to set up
  or load the brain, describe it first, from the README and manifests: `project describe -` (what the app is, its
  services and how they connect, conventions they share: the big picture, in at most five lines, without tables,
  columns, function signatures, files or dependencies, which the code already says). Rewrite it when the big
  picture changes (a service added, another storage), not after every change. With several repositories, this
  one's role too: `project notes -`. How to build, test and run this repository, and its code style, go in this
  file, outside this section: add them as you learn them.
- Also: `show ID` (one entry in full), `describe ID TEXT`, `rename ID TITLE`, `tag ID +TAG -TAG` (add or remove
  tags), `link ID OTHER…` (an id here, or `REPO#ID` in another repository of the project); `-` reads text from
  stdin. Never edit the files in `.brain/` by hand: `kb` keeps them consistent.
<!-- kanbrain:end -->
