# Error reports

A report is a small JSON file that describes one failure so it can be debugged
without the player's logs. Reports are meant to become public pages on the
receiving server (decisions #985, #988, #989), so a report is built to hold
nothing private rather than cleaned afterwards.

Status: the client builds, previews, saves and, when the player asks, sends reports
to the report server (`crates/reports-server`, decisions #1041, #1043, #1046).
Nothing is ever sent automatically: every send is one press and one confirmation,
and there are no retries or background sends.

## What a report contains

The schema is closed (`crates/report`, `schema.rs`, version 2): every struct
refuses unknown fields, so a field that is not named there cannot travel.

- `product`: `decanter` or `psport` (#991).
- `date`: the day (UTC), never the time.
- decanter's version and commit; the system family and release; the GPU
  vendor, model and driver (psport reports, from the run's log).
- A psport report adds the game: title id, name and version from the dump's
  `param.sfo`, whether psport applied a profile of its own, the run settings
  that change how it runs (`PS_*` allow-list), the failure's call report
  (name, code, count), the fault (signal and image-relative offsets) and the
  frame statistics.
- A decanter report adds the action that failed (install, launch, sync,
  settings, build, update).
- `failure`: its kind (`crash`, `stop`, `error`, `exit`, `missing`), how many
  times it repeated, and the message.

## What it never contains

Paths, user names, host names, profile ids, account names or ids, display and
gamepad names, window titles, addresses, times of day, tokens, raw log lines.

Three layers keep them out:

1. **Allow-list.** Only the fields above exist, and each one is checked against
   its own shape (`validate`). The raw `failures.jsonl` fields that hold paths
   and output (`game_dir`, `log`, `before`, `after`) are never deserialized.
2. **Closed vocabulary (#1034).** `failure.message` is not text: it is one of
   the known templates of psport's and decanter's messages with typed slots (a
   call name, a code, an offset, an image name). A line that fits no template is
   reported by its kind alone.
3. **Identity (#1034).** The client knows what the server cannot: the names of
   this machine and person. Before a report is built, decanter collects them and
   the builders refuse or drop any value that holds one. The server never
   applies this list, since it has none; it validates the same schema and
   vocabulary.

The identity list is made of: `USER` / `USERNAME`, the host name, the home
folder and the folders of every known library below it, the profile names, the
GOG account names, Steam's persona and account names (`loginusers.vdf`), the
shadNet NPID and the saved gamepad name. The display names, the PSN names a game
reports on its own and the account's full name are not collected: decanter has
no code that reads them.

A report that the rules reject is not produced: the preview and the CLI show the
field and the rule that failed, never the value.

## Building and saving

- Command line: `decanter ps report`. It reports the newest failure of the active
  profile by default; `--title CUSA…` and `--failure N` (the newest is 1) choose
  another; `--product decanter` reports a failure of decanter's day log (`--day
  YYYY-MM-DD` picks the day). `--print` shows the JSON as it would go out and
  saves only when `--out FILE` is also given.
- Window: **Reportar errores** on a failure's details in PS, Fallos, and on the
  banner of a failed run on the game's page. The panel shows the exact JSON and
  **Guardar informe** saves it; nothing is sent from there.
- The default file is `<library>/decanter/reports/<date>-<fingerprint>.json`,
  inside the library like the rest of decanter's data. An existing file is never
  replaced. The fingerprint groups the same failure on the same commit.

## Recording a failure without the window

`decanter ps run --title CUSA…` plays a game the way the window's Play does (the same port,
settings, environment and scan of the output) and appends what the run left to the active
profile's failures, the record Fallos and `decanter ps report` read. So a test on another machine
runs, then returns its failure with `decanter ps report --title CUSA… --print`. Error lines and
missing calls are recorded but leave the status `clean`, as they leave Play's banner a notice; a
crash, a `STOP:` or a bad exit makes it `failure` and the exit code 1. A run ended by
`--timeout` is a `timeout`: the stop it asked for is not recorded as a crash, and the run log
says when the timeout came and whether the game had to be killed. The command never connects to
online services and never prepares a port or converts a game; on Windows it runs the `psport.exe`
Play would (the folder picked in Ajustes, else the build or package in use) and never downloads
one. See the README's CLI section.

## Sending

The report that was previewed, byte for byte, goes to `POST /v1/reports` on the report
server, which validates it again with the same crate and publishes it as a public page. The
player gets back the page's address and a **delete code**.

- Window: under the preview, the privacy notice and a **Enviar informe** button that asks for a
  last confirmation. Afterwards the panel shows the public address (copy, open), the code, and
  **Borrar informe enviado**. The button is hidden when sending is turned off in the settings.
- Command line: `decanter ps report --send` shows the report and what would be sent, and sends
  only with `--yes`; without it nothing touches the network. `--attach-log` (with `--send --yes`)
  adds the log. `--sent` lists what this library has sent (never the codes) and
  `--delete ID --yes` deletes one, using the code kept in the library. A hidden `--server URL`
  replaces the configured address, with the same restrictions, for development.
- A report with the same fingerprint and day as one already sent is not sent again: the answer
  is the earlier report's address. Delete it first to send it anew.
- Errors are shown as the server gave them: a rejected report lists every field and rule (never
  a value), a rate limit says how long to wait, and a network failure shows its message.

### The server and the setting

The address is compiled in (`https://reports.yerasuu.com`) and can be replaced by the advanced
setting `report_server` in decanter's settings (the `settings` table of the library's database).
An empty value turns sending off. There is no environment variable for it. Only `https://`
addresses are accepted, plus `http://127.0.0.1:PORT` and `http://localhost:PORT` for a local
server. Redirects are never followed, the request times out after 30 s and the answer is read up
to 16 KiB. The answer is checked before anything is kept: the id is `d` (decanter) or `p`
(psport) plus 32 lowercase hex characters and matches the report's product, the delete code is
32 hex characters, the fingerprint is 16 hex characters, and the address is the configured host
with the path `/r/<id>`; otherwise the send counts as failed and nothing is stored.

### The registry and deletion

`<library>/decanter/reports/sent.jsonl` holds one line per sent report: `id`, `delete_code`,
`url`, `fingerprint`, `product` and `day`. The file is created readable by its owner only
(0600 where the platform supports it) because the delete code is in it. Deleting asks the server
that holds the report (the one in its `url`) with the code; the line is then removed. If the
server no longer knows the report (for instance after its retention) the line is removed too.
Neither the code nor the report is ever written to decanter's logs.

### The optional log

A checkbox, **Adjuntar el registro completo para quien mantiene (no se publica)**, off by
default, sends the failure's run log after the report was accepted, with the report's delete
code. Before it leaves, the log is passed through the identity list (user, host, home folders,
accounts, profile names: `decanter_report::scrub_log`) and cut to its last 5 MiB. That list is
best effort: the log may hold names it does not know, which is why the file is private. The
server stores it encrypted, only whoever maintains the server can read it, there is one per
report, and it is deleted when the report is resolved or deleted. If the log cannot be attached
the report stays sent and the player is told so.

### Privacy notice

The same text the window shows before the confirm button:

> The report is public and is kept until it is resolved. It holds no paths, names or accounts.
> You can delete it with the code you will get. There is no account, and your IP is kept only as
> long as the rate limit needs it.

In Spanish: *El informe es público y se conserva hasta que se resuelva. No lleva rutas, nombres
ni cuentas. Podrás borrarlo con el código que recibirás. No hay cuenta, y no se guarda tu IP
salvo lo necesario para limitar el ritmo de envíos.*
