# Git conventions (portable)

The commit, branch, push and version rules used across these projects. Copy the "CLAUDE.md
section" into a project's CLAUDE.md, install the hook, and adapt the two marked places (the
scope list and what counts as breaking).

## CLAUDE.md section

````markdown
## Commits and branches

- Branches: `main` (what users get, a tag per version), `develop` (integration),
  `<type>/<task id>-<topic>` per task (`feat/42-sync-saves`, `fix/57-crash-on-exit`).
- Into `develop`: rebase the branch on `develop`, squash its checkpoints into one useful commit
  per task, fast-forward `develop` (linear history, no merge commits). Into `main`: fast-forward
  from `develop` once the change is validated (tests, night runs, whatever the project uses).
- Never merge `main` into a work branch; always rebase.
- One useful commit per task or feature, carrying everything the change needs (code, docs,
  comments, locale strings, tests or test schedules). Split only when each part stands on its
  own, e.g. a large mechanical move kept apart from the behaviour change after it. Fewer, more
  useful commits over many small ones.
- Always Conventional Commits:

      type(scope): subject

      Optional body of one to four lines with the why when it is not obvious:
      the cause, the evidence, the measured effect.

      BREAKING CHANGE: what users or launchers must change (only when it breaks).

  - `type`: `feat`, `fix`, `perf`, `refactor`, `style`, `docs`, `test`, `build`, `ci`, `chore`;
    `!` after the scope (`feat(app)!:`) when it breaks.
  - `scope`: the area touched. <ADAPT: list this project's areas, e.g. app, cli, core, docs.>
  - Subject: imperative English, ~50 characters, 72 at most, no trailing period.
  - Body: after a blank line, lines of 72 characters (100 at most). Don't repeat what the issue,
    the board or the docs already say.
- Commits and tags are signed.
- Never a `Co-Authored-By` trailer or any AI-attribution line, in commits or PR descriptions.
  Caught by `.githooks/commit-msg` (`git config core.hooksPath .githooks`).
- Pushing is the owner's job. What is published is never rewritten (amend, squash, rebase,
  force push) unless the owner orders it; `git fetch` and check `origin/<branch>` before any
  history rewrite. `--force-with-lease` only on your own unpublished work branches.

## Versioning

SemVer derived from the commits in the range, never by feel:

| Commits in the range | Bump (`>= 1.0`) | Bump (`0.y.z`) |
|---|---|---|
| anything breaking (`!`, `BREAKING CHANGE:`) | major | `0.(y+1).0` |
| `feat`, `fix`, `perf` | minor | `0.y.(z+1)` |
| only `docs`/`chore`/`style`/`refactor`/`test`/`ci`/`build` | no release | no release |

- What breaks: <ADAPT: for an application, its stored data, settings and CLI; for a server,
  flags, config keys, routes and storage layout; for a library, its exported API.>
- A dependency repin alone is a `chore` and earns no version.
- Release order: write the version's notes first (`tag-changelog/vX.Y.Z.md`, with the build
  failing without them), then the version bump as its own commit, then fast-forward `main`, then
  the annotated, signed tag `vX.Y.Z` on `main`.
- Tag notes are a technical record: terse bullets anchored in code (module, function, setting,
  path, constant and value), read from the tree at that tag. `CHANGELOG.md` is the other half,
  grouped by area.
````

## The hook

Save as `.githooks/commit-msg` (executable, `chmod +x`) and enable it in each clone with
`git config core.hooksPath .githooks`. It needs only bash and grep.

```bash
#!/usr/bin/env bash
# Enable with: git config core.hooksPath .githooks

types='feat|fix|perf|refactor|style|docs|test|build|ci|chore'
subject_re="^(${types})(\([a-z0-9-]+\))?!?: .+"
ai_re='co-authored-by|generated with|claude|anthropic|chatgpt|openai|copilot|ai assistant'

reject() {
    printf 'commit-msg: %s\n' "$1" >&2
    exit 1
}

lines=()
while IFS= read -r line || [ -n "$line" ]; do
    case $line in
        '# ------------------------ >8 ------------------------'*) break ;;
        '#'*) continue ;;
    esac
    lines+=("${line%$'\r'}")
done < "$1"
while [ ${#lines[@]} -gt 0 ] && [ -z "${lines[-1]//[[:space:]]/}" ]; do
    unset 'lines[-1]'
done
[ ${#lines[@]} -eq 0 ] && exit 0

for line in "${lines[@]}"; do
    # CLAUDE.md and .claude/ are file names in the tree, not attribution.
    if printf '%s\n' "${line//CLAUDE.md/}" | sed 's#\.claude/##g' | grep -Eiq -- "$ai_re"; then
        reject "no AI attribution or Co-Authored-By lines: '$line'"
    fi
done

subject=${lines[0]}
case $subject in
    'Merge '* | 'Revert "'* | 'fixup! '* | 'squash! '* | 'amend! '*) exit 0 ;;
esac

printf '%s\n' "$subject" | grep -Eq -- "$subject_re" ||
    reject "subject must be 'type(area): subject' with type one of ${types//|/, }"
[ ${#subject} -le 72 ] || reject "subject is ${#subject} characters, at most 72"
if [ ${#lines[@]} -gt 1 ]; then
    [ -z "${lines[1]}" ] || reject "leave the second line empty before the body"
    for line in "${lines[@]:2}"; do
        [ ${#line} -le 100 ] || reject "body line is ${#line} characters, at most 100: '${line:0:40}...'"
    done
fi
exit 0
```

## Setting up a repository

1. Add the CLAUDE.md section and fill the two `<ADAPT: …>` places.
2. Add `.githooks/commit-msg` and run `git config core.hooksPath .githooks`.
3. Create `develop` from `main` and publish it: `git push -u origin develop`.
4. Signing: `git config commit.gpgsign true` and `git config tag.gpgsign true` (with
   `user.signingkey` set), or the same options globally.
5. Optional, for a project whose history should follow the format from the start: regroup the
   published history into useful commits on a new `main`, keeping trees, dates and signatures,
   with the old branch archived in a tag (`archive/<branch>-<date>`). Only on the owner's order.
