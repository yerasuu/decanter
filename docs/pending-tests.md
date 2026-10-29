# Pending tests (2026-10-29)

Who runs what, where, with which limits. Updated by hand; the board entry named in each row holds the history.

| Job / entry | Who, where | Test | Limits and rules | State |
|---|---|---|---|---|
| j229 | Windows agent | Repeat j220: build psport `52e552e913`, measure Bloodborne | Done (j237): builds; Bloodborne faults at `0xf6f357`, fixed in #1244 | done |
| j231 | Windows agent | decanter `a1bd821`: select look, `ps run` with Bloodborne, unknown title | Done (j238): selects ok; `ps run` refused on Windows, fixed in #1243 | done |
| #1243 | Windows agent | `ps run` on Windows: unknown title, nothing installed, run with timeout, no `psport.exe` left, `--env PS_TIMEOUT`, `ps report`, Play and DETENER regression | `--timeout 90`; no network; kill lands about 10 s after the timeout | needs push |
| #1239 | Windows agent | Ports pruning: only the build in use stays, failed build log and Retry, Download and use | no network except the user's build/download buttons | needs push |
| #1244 | Windows agent | Bloodborne with the SysV hooks: fault at `0xf6f357` gone, `PS_GAME_MENU=0` as control | no network | needs push |
| #1245 | agent-wbc3 | Sweep `PS_VRAM_LIMIT_MB` 2560, 2816, 3072 and one unlimited run | `regress-perf`, one at a time | needs push |
| j230 | agent-wbc3 | `decanter ps run` with Saga and Bloodborne, then `ps report` | `--timeout 150` and `--timeout 90`; never online; needs decanter `6279c56` and the `decanter-run` policy change (policy-proposed9.toml, owner's decision) | blocked on policy |
| #1218 | agent-wbc3 | More Saga runs for the `0x20067d4` fault after `heap error is detected` | One at a time, alone, `PS_FRAME_STATS=1`, no network | pending analysis |
| #1181 | agent-wbc3 | Texture cache budget sweep | Done (j239, j241): 4096 and 3072 keep 60 FPS, 2560 and 2048 lose about 28%, 1024 and below collapse; follow-up in #1245 | done |
| #1237 | owner | Install a game with one library and no branches (no dialog), and one with branches or two folders (dialog, choice respected) | By hand in `dev` | waiting |
| #1229 | owner | Forced refresh, use after the access token expires, cloud saves sync | Real account, by hand | waiting |
| #1185, #1186, #1232 | owner | Steam Deck, online play, select look | By hand | waiting |

## Common limits

- No agent tests online; only the owner does.
- Game runs always carry `--timeout`; Ctrl-C leaves the game running.
- On the owner's machine games run by day only when the owner orders it; on agent-wbc3 at any hour.
- agent-wbc3 updates psport to `origin/develop` before each batch.
- agent-wbc3 runs only what its `allow_run` and `env_allow` list.
