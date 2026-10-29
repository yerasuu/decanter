# Pending tests (2026-10-29)

Who runs what, where, with which limits. Updated by hand; the board entry named in each row holds the history.

| Job / entry | Who, where | Test | Limits and rules | State |
|---|---|---|---|---|
| j229 | Windows agent | Repeat j220: build psport `52e552e913`, measure Bloodborne | No network; a failed compile sends the error lines and stops | queued |
| j231 | Windows agent | decanter `a1bd821`: select look, `ps run` with Bloodborne, unknown title | `--timeout 90`; no network; on Windows the timeout only sends the stop (no hard kill) | queued |
| j230 | agent-wbc3 | `decanter ps run` with Saga and Bloodborne, then `ps report` | `--timeout 150` and `--timeout 90`; never online; needs decanter `6279c56` and the `decanter-run` policy change (policy-proposed9.toml, owner's decision) | blocked on policy |
| #1218 | agent-wbc3 | More Saga runs for the `0x20067d4` fault after `heap error is detected` | One at a time, alone, `PS_FRAME_STATS=1`, no network | pending analysis |
| #1181 | agent-wbc3 | Texture cache budget with `PS_VRAM_LIMIT_MB` 2048 and 4096 | `regress-perf` profile only; 512 MiB already measured (8.5 FPS) | proposed |
| #1237 | owner | Install a game with one library and no branches (no dialog), and one with branches or two folders (dialog, choice respected) | By hand in `dev` | waiting |
| #1229 | owner | Forced refresh, use after the access token expires, cloud saves sync | Real account, by hand | waiting |
| #1185, #1186, #1232 | owner | Steam Deck, online play, select look | By hand | waiting |

## Common limits

- No agent tests online; only the owner does.
- Game runs always carry `--timeout`; Ctrl-C leaves the game running.
- On the owner's machine games run by day only when the owner orders it; on agent-wbc3 at any hour.
- agent-wbc3 updates psport to `origin/develop` before each batch.
- agent-wbc3 runs only what its `allow_run` and `env_allow` list.
