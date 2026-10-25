# Error reports

A report is a small JSON file that describes one failure so it can be debugged
without the player's logs. Reports are meant to become public pages on the
receiving server (decisions #985, #988, #989), so a report is built to hold
nothing private rather than cleaned afterwards.

Status: the client builds, previews and saves reports. **Nothing is sent**: the
server (#993) does not exist yet, and sending arrives with it, with a button the
player presses after seeing exactly what goes out (#982).

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
- Window: PS, Fallos, a failure's details, **Informe de errores**. The panel shows
  the exact JSON (nothing is sent) and **Guardar informe** saves it.
- The default file is `<library>/decanter/reports/<date>-<fingerprint>.json`,
  inside the library like the rest of decanter's data. An existing file is never
  replaced. The fingerprint groups the same failure on the same commit.
