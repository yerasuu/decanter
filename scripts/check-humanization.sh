#!/usr/bin/env bash
set -euo pipefail

# Pattern scanner for AI-code fingerprints not caught by Clippy.
# Exits non-zero on violations. Intended for pre-commit and CI.

errors=0
report() {
    echo "HUMANIZE VIOLATION ($1): $2"
    errors=$((errors + 1))
}

# Local pre-commit hook: scan what's staged. In CI there's nothing staged
# on a plain checkout, so diff the PR against its base branch instead.
if [ -n "${GITHUB_BASE_REF:-}" ]; then
    git fetch --depth=1 origin "$GITHUB_BASE_REF" >/dev/null 2>&1
    range="origin/${GITHUB_BASE_REF}...HEAD"
    files=$(git diff --name-only --diff-filter=ACM "$range" -- '*.rs')
else
    range="origin/master..HEAD"
    files=$(git diff --cached --name-only --diff-filter=ACM -- '*.rs')
fi
[ -z "$files" ] && exit 0

for f in $files; do

    # 1. Comments that restate the code without adding WHY.
    #
    # Flagged: "// Iterate over items" (action verb, no causal word)
    # Allowed: "// caches for 5 min because upstream poll interval changed" (has "because")
    # Allowed: "// SAFETY: pointer validated" (human marker)
    # Allowed: "// TODO: batch once #118 lands" (human marker)
    #
    # Strategy: flag if line has action verbs AND NO causal/human marker words.
    # The human marker list matches CLAUDE.md exactly.
    #
    if grep -nE '^\s*//' "$f" \
       | grep -iE '\b(returns?|gets?|sets?|creates?|checks?|processes?|iterates?|loops?|calls?|parses?|converts?|transforms?|handles?|stores?|loads?|saves?|updates?|deletes?|removes?|adds?|appends?|pushes?|pops?)\b' \
       | grep -ivE '\b(because|since|so that|so|in order to|otherwise|if not|when|unless|due to|after|before|once|until|to avoid|to prevent|to ensure|for now|temporarily|workaround|hack|hotfix|SAFETY|TODO|FIXME|NOTE|XXX|WARN|#\d+|issue|bug|invariant|constraint|assumes?|guarantee|contract|caller|must|should never|must not|race|deadlock|overflow|underflow|panic|undefined)\b' \
       >/dev/null 2>&1; then
        report "obvious-comment" "$f — line comment restates WHAT without explaining WHY"
    fi

    # 2. Doc comments that only list arguments without contractual info.
    #
    # Flags "# Arguments" section followed by "* `name` - The ..." where
    # NONE of the arguments carry contractual keywords (must, ownership,
    # borrowed, valid, non-empty, etc.).
    #
    # Does NOT flag if ANY argument has contractual info — the whole
    # function passes, no partial penalty.
    #
    if grep -nE '^///\s*#\s*[Aa]rguments' "$f" >/dev/null 2>&1; then
        # Found an # Arguments section. Check if all its argument lines
        # are pure type restatements ("The first number") vs contractual
        # ("must be non-empty", "ownership transferred").
        has_contractual=$(sed -n '/^\/\/\/\s*#\s*[Aa]rguments/,/^\/\/\/\s*#/p' "$f" \
           | grep -iE '`[a-z_]+`\s*[-:]' \
           | grep -iE '(must|should|may not|cannot|ownership|transferred|borrowed|valid|non-empty|null|uninitialized|aligned|exclusive|shared|lifetime|outlives|dereference)')
        
        if [ -z "$has_contractual" ]; then
            report "doc-restates-signature" "$f — # Arguments section restates types without contractual info"
        fi
    fi
    # 3. Type annotations on let bindings where inference is trivial.
    #    "let x: u32 = 0;" or "let name: String = ..." without a cast.
    if grep -nE 'let\s+mut?\s+\w+\s*:\s*(u8|u16|u32|u64|i8|i16|i32|i64|f32|f64|bool|String)\s*=' \
       "$f" | grep -vE '(as|From|from|into|parse|try_from)' >/dev/null 2>&1; then
        report "trivial-type-annotation" "$f — type annotation on primitive let binding"
    fi

    # 4. Explicit lifetimes where elision applies.
    #    "fn foo<'a>(x: &'a str) -> &'a str" with single input/output.
    if grep -nE 'fn\s+\w+<\s*'\''a\s*>\s*\([^)]*&\s*'\''a\s' "$f" >/dev/null 2>&1 \
       && ! grep -q 'unsafe\|extern\|dyn' <<< "$(grep -E 'fn\s+\w+<\s*'\''a\s*>' "$f")"; then
        report "explicit-lifetime-elision" "$f — lifetime can be elided"
    fi

    # 5. Clone on a value that was just borrowed.
    #    "items.iter().map(|x| x.clone())" or "self.field.clone()"
    #    near a reference parameter.
    if grep -nE '\.clone\(\)\s*$' "$f" | grep -vE '_owned|_cloned|to_send|Arc|Rc' >/dev/null 2>&1; then
        report "possible-borrow-instead-of-clone" "$f — .clone() on a possibly borrowable value (review)"
    fi

    # 6. block_on inside async context.
    if grep -nE 'block_on\s*\(' "$f" | grep -iv 'test\|#\[test\]' >/dev/null 2>&1; then
        report "block-on-in-async" "$f — block_on() inside async code, use .await instead"
    fi

    # 7. Generic println! in non-test code (use tracing instead).
    if grep -nE 'println!\s*\(' "$f" | grep -v '#\[cfg(test)\]\|mod tests\|#\[test\]' >/dev/null 2>&1; then
        report "println-in-production" "$f — println! in production code, use tracing instead"
    fi

    # 8. Catch-all wildcard match on closed enums.
    #    "Err(_) => " when the error variant is known and few.
    if grep -nE 'Err\(_\)\s*=>' "$f" >/dev/null 2>&1; then
        report "wildcard-error-match" "$f — Err(_) catches all variants, handle explicitly or document why"
    fi

    # 9. Vec or HashMap behind Mutex when atomics or RwLock might suffice.
    #    Heuristic: Mutex<u32>, Mutex<i64>, etc.
    if grep -nE 'Mutex<(u8|u16|u32|u64|i8|i16|i32|i64|usize|isize|bool)>' "$f" >/dev/null 2>&1; then
        report "mutex-on-primitive" "$f — Mutex on primitive, use atomics (AtomicU32 etc.)"
    fi

    # 10. #[derive()] with more than 4 traits — likely copy-paste AI.
    if grep -nE '#\[derive\(([^)]*,){4,}' "$f" >/dev/null 2>&1; then
        report "excessive-derives" "$f — more than 4 derives, review which are actually needed"
    fi

done

# Git-layer checks (not file-scoped).

# 11. No AI co-author or AI attribution in commit messages.
for msg in $(git log --format='%H' "$range" 2>/dev/null || git log --format='%H' -5); do
    body=$(git log -1 --format='%B' "$msg")
    if echo "$body" | grep -iE 'co-authored-by.*(claude|copilot|chatgpt|openai|gemini|cursor|aider)' >/dev/null 2>&1; then
        report "ai-attribution" "commit $msg contains AI co-author tag — remove it"
    fi
done

if [ "$errors" -gt 0 ]; then
    echo ""
    echo "Total violations: $errors"
    echo "Fix these before committing. See CLAUDE.md — Filosofía."
    exit 1
fi