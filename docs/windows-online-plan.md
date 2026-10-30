# Online play on Windows: plan

Recorded on 2026-10-30 for #1296 (prior work the owner asked for). Read-only
analysis: nothing was built, run or connected. psport paths are relative to
psport's checkout (`develop`, `c74e3d3` or later); decanter paths to this
repository. "Unverified" marks what reading the code could not settle.

Today online play exists only on Linux. On Windows decanter's table says
`PsOnline` is `NotOnThisSystem` (`crates/core/src/platform/windows.rs:219`),
so the game's Online tab and Ajustes' online section are hidden (#1294,
`Caps::is_never_here`). Agents never test online (decision #601): every check
below is offline or on `127.0.0.1`; only the owner plays online.

## 1. The Linux path today

### decanter

| Step | Where | What | Linux-only |
|---|---|---|---|
| Settings | `crates/ps/src/bbport/online.rs:24-33` | `online`, `online_server`, `online_webapi`, `online_community`, `online_npid`, `online_upnp`, `online_bbsc`, `online_bbsc_local_server`, as psport's launcher names them | no |
| Checks | `online.rs:92-232` | host, `host:port`, http(s) URL, NPID (3-16 of `[A-Za-z0-9_-]`, no `@`), secrets (no NUL, 1024 bytes max) | no |
| Secrets | `crates/core/src/credentials.rs:120-180`, `app/src/ps_play/launch.rs:278-317` | password and token under `shadnet/<NPID>/{password,token}` in the keyring (decision #715); a launch with an NPID and no password stops | backend only (Secret Service; see 2.7) |
| Environment | `online.rs:286-308`, `crates/ps/src/bbport/mod.rs:455` | `PS_ONLINE=1`, `PS_SHADNET_SERVER`, `PS_SHADNET_WEBAPI`, `PS_COMMUNITY_SERVER`, `PS_UPNP`, `PS_BBSC`, `PS_BBSC_LOCAL_SERVER`, `PS_SHADNET_NPID`, `PS_SHADNET_PASSWORD`, `PS_SHADNET_TOKEN` | no: `game_env` is shared with the direct launch (`mod.rs:347`) |
| Title data | `online.rs:343-382` | `psport --online-overrides --title ID`, for the extension's name (the BBSC switches, `app/src/views/ps/games/online.rs:82`) | yes: runs through `crate::os::linux::toolchain` |
| Capability | `crates/ps/src/bbport/features.rs:27,35,105-109`, `app/src/capabilities.rs:163-168` | `PsOnline` off until `--list-features` lists `online_module`; follows the `net` rows | yes: `list_features` (`features.rs:211-235`) runs through the toolchain |
| Connection test | `online.rs:387-487` | reqwest GET of the game server's `ss.info` and the WebAPI's `/status`, a tokio TCP connect to shadNet; only on a click | no |

### run.sh (`run.sh:281-329`, only with `PS_ONLINE=1`)

1. `285-287`: warns when `gpu/libpsport-net.so` is missing (the game then
   plays offline).
2. `288-297`: finds Python 3 (`PYTHON`, `python3`, `/nix/store/*-python3-*`)
   or exits.
3. `298`, `307`: defaults `PS_SHADNET_SERVER=play.seamlesshunt.com:31313`
   and `PS_SHADNET_WEBAPI=http://<server host>:31315`.
4. `302-306`: `SSL_CERT_FILE` from `NIX_SSL_CERT_FILE` or the usual PEM
   bundles under `/etc`, because mbedTLS reads no system store on Linux
   (`gpu/net/psport_net_tls.h:7-16`).
5. `308`: `psport --online-overrides --game DIR` (`src/main/cli.c:153-174`),
   the title's `key=value` lines from `gpu/games/<game>/title.cpp`
   (Bloodborne: `gpu/games/bloodborne/title.cpp:130-139`).
6. `313`: `scripts/online.py local-server` decides whether the title's game
   server runs on this PC (`scripts/online.py:149-197`: the extension switch
   `PS_BBSC=1`, `PS_BBSC_LOCAL_SERVER` `1`/`auto`; `auto` probes the
   community server's status path with a 5 s HTTP GET).
7. `316-324`: if nothing answers on `127.0.0.1:<port>` (bash `/dev/tcp`),
   starts `scripts/bbsc_gameserver/bbsc_gameserver.py` in the background with
   a watcher loop (`kill -0`) that stops it once the game's process is gone;
   `325` then points `PS_COMMUNITY_SERVER` at `http://127.0.0.1`.
8. `328`: `scripts/online.py overrides` writes `<user>/host_overrides.json`
   (`online.py:94-118`): the title's `game_server` to the community server,
   and the summon route to the shadNet WebAPI when a POST to
   `<webapi><route>/create` does not answer 404 (`online.py:80-91`, a 4 s
   network probe); `host_overrides.custom.json` wins when present. Its path
   goes to `SHADPS4_HTTP_HOST_OVERRIDES_JSON`.
9. `331-332`: `exec` of the program.

### psport runtime (`psport`, C)

- `src/net/runtime_net.c:24-28`: `PS_ONLINE=1` turns online on;
  `31-41` routes `sceNet*`, `sceNp*`, `sceHttp*`, `sceSsl*` to the module,
  except trophies, commerce and the profile dialog.
- `runtime_net.c:65-73`: the module is `PS_NET_MODULE` or
  `<exe dir>/gpu/libpsport-net.so`, a hard-coded Linux name and folder.
- `runtime_net.c:77-87`: `--list-features` probes the file's presence
  without loading it; `89-120` loads it (`ps_host_dl_open`), calls
  `psport_net_init(PsNetHost)` (ABI 3, `gpu/net/psport_net.h:13-34`), then
  `psport_net_resolve` per import.
- `src/platform/host_features.inc:279-284`: `online_module` has a `LINUX`
  row only; an OS without a row counts it unavailable
  (`src/platform/host_features.c:104`).

### Online module (`gpu/net`, C++, `libpsport-net.so`)

- Build: `gpu/CMakeLists.txt:284-312` builds it when Python 3,
  nlohmann_json, MbedTLS, ZLIB and miniupnpc are found;
  `gpu/net/CMakeLists.txt:11-18` runs `proto/gen_proto.py` (Python) to
  generate the shadNet messages; `53-58` are ELF-only (`-Bsymbolic`,
  `--exclude-libs`, `$ORIGIN` rpath); `59-62` links `psport-gpu`, mbedTLS
  3.6, zlib, miniupnpc.
- Settings from the environment: `gpu/net/include/core/emulator_settings.h:78-88`
  (`PS_ONLINE`, `PS_SHADNET_SERVER`, `PS_SHADNET_WEBAPI`, `PS_UPNP`) and
  `gpu/net/include/core/user_manager.h:31-44` (`PS_SHADNET_NPID`,
  `PS_SHADNET_PASSWORD`, `PS_SHADNET_TOKEN`, `PS_NP_COUNTRY`, ...).
- TLS: cpp-httplib 0.45.0 over mbedTLS (kb #850); `PsNetTls::UseCaBundle`
  (`psport_net_tls.h:14-18`) sets `SSL_CERT_FILE` when given, at
  `np_handler.cpp:621,723` and `http.cpp:765`.
- Host overrides: `shadps4/core/libraries/network/http.cpp:298-318` reads
  `SHADPS4_HTTP_HOST_OVERRIDES_JSON`, else `<user dir>/host_overrides.json`.
- Sockets: the module calls the host's sockets itself, not psport's
  `ps_host_net_*` (that layer serves the offline `src/net/runtime_socket.c`,
  #614). The inherited shadPS4 code already has `_WIN32` branches:
  `network/sockets.h:6-46`, `net_util.cpp:4-23`, `netctl.cpp:4-13`,
  `shadnet/client.h:17-36`, `p2p_sockets.cpp:973-985` (a readiness socket
  pair on Windows, `eventfd` on Linux), `net_epoll.h:15-17` (`wepoll.h` on
  Windows).
- Lines psport added that are Linux-only: the epoll abort `eventfd`
  (`net_epoll.h:45-51,65-72`), `sceNetEpollAbort` (`net.cpp:648-669`,
  a `LOG_TRACE` stub elsewhere), the `epoll_pwait` deadline loop and its
  `EINTR` (`net.cpp:911-966`; the other branch passes microseconds divided
  to milliseconds).
- UPnP: miniupnpc (`network/net_upnp.cpp:73-148`): discovery, IGD, UDP
  port mapping and its removal, unless `PS_UPNP=0`.
- P2P: UDP on `SHADPS4_P2P_PORT` or an ephemeral port
  (`p2p_sockets.cpp:186-196`), relay through `SHADPS4_P2P_RELAY`.
- shadNet: plain TCP (`shadnet/client.cpp:303,324`); the login sends the
  password in the protobuf request (`client.cpp:206-208`).
- Bloodborne's extension `bbsc` (`gpu/games/bloodborne/net/`, about 17 400
  lines) is built into the module; no POSIX header or `__linux__` found in
  it.

### Local game server

`scripts/bbsc_gameserver/` (`bbsc_gameserver.py` 1174 lines,
`bbhost_service.py` 790 lines), Python standard library only
(`ThreadingHTTPServer`), Bloodborne's: serves `ss.info` and empty answers,
forwards the four summon endpoints to shadNet's WebAPI. Its own README
already describes running it on Windows with Python.

### Ports

| Port | Proto | Who |
|---|---|---|
| 31313 | TCP | shadNet server (out) |
| 31315 | HTTP | shadNet WebAPI (out, and the summon override) |
| 20443 | HTTP(S) | the title's game server; the local server listens on `127.0.0.1` only |
| 18671 | HTTP | community API port (`title.cpp:132`) |
| P2P | UDP | in and out; the port UPnP maps |

## 2. Windows equivalents

### 2.1 Building the module

A DLL, `libpsport-net.dll`, not a static part of `psport.exe` or
`libpsport-gpu.dll`: the runtime already loads it only for online play and
probes it by presence (`runtime_net.c:77-120`), `LoadLibraryExW` with
`LOAD_WITH_ALTERED_SEARCH_PATH` already exists
(`src/platform/win32/host_library.c:8-25`), and a separate file keeps the
licence split of #851 (the module out of the public bundle until the owner
says otherwise). `tools/win32-bundle.sh:185-187` already expects
`$out/gpu/libpsport-net.dll` and leaves it out.

What changes, psport:

- `CMakeLists.txt:35` forces `PS_NET OFF` for the Windows entry point and
  `tools/win32-cross.sh:240` passes `-DPS_NET=OFF`: become the option.
- The CLANG64 packages: `.github/workflows/windows.yml`'s list (which
  `tools/win32-cross.sh:71-75` and the toolchain package read) gains
  `mbedtls`, `miniupnpc`, `nlohmann-json`, `zlib`. That is a new
  `toolchain-win32-N`, pinned again in decanter
  (`crates/ps/src/os/windows/winbuild/pins.rs:13-17`). Unverified: the
  package names and versions MSYS2 ships, whether each has a static
  library, and whether MSYS2's mbedTLS is 3.6 as `CPPHTTPLIB_MBEDTLS_V3`
  needs. Static linking of the four is preferred: no new DLLs to vet.
- Python at build time: decanter's Windows build (`winbuild/mod.rs:533-551`)
  has no Python, and `gen_proto.py` needs one. Either the generated
  `shadnet.pb.{h,cc}` are committed with a CI check that regenerating gives
  the same bytes, or the toolchain package carries Python. Committing is
  smaller.
- `gpu/net/CMakeLists.txt:53-58`: the ELF flags under `if(NOT WIN32)`.
  The three entry points (`psport_net_init`, `psport_net_resolve`,
  `psport_net_image_ready`) exported by a `.def`, because MinGW exports
  every symbol of a DLL that declares none (unverified how lld treats
  `-fvisibility=hidden` here: checked with `llvm-objdump -p` after the
  link).
- Imports: shadPS4's common code (logging, thread names) from
  `libpsport-gpu.dll` through `libpsport-gpu.dll.a`; `ps_host_process_id`
  (`psport_net_glue.cpp:310,316`) from `psport.exe`, which
  `gpu/port/psport_runtime.def:29` already lists, through the import
  library `gpu/CMakeLists.txt:243-250` makes. Unverified until the first
  link: that `libpsport-gpu.dll` exports every symbol the module takes from
  it (`gpu/shadps4/common/memory_patcher.h:10` uses `dllexport`, which turns
  MinGW's auto-export off for the objects that see it).
- `ws2_32`, `iphlpapi` (net_util, netctl, miniupnpc) and `crypt32`
  (cpp-httplib's Windows store, below) linked explicitly; cpp-httplib's
  `#pragma comment(lib, "crypt32.lib")` (`httplib.h:380`) is an MSVC
  convention.
- Location: `libpsport-gpu.dll` sits beside `psport.exe` in the package
  (`tools/win32-bundle.sh:34,184`), not in `gpu/`. `runtime_net.c:72` takes
  the module's name and folder from the platform layer (Linux
  `gpu/libpsport-net.so`, Windows `libpsport-net.dll` beside the exe), and
  `win32-bundle.sh` and decanter's `winbuild/assemble.rs` copy it with its
  DLL closure when the owner releases it.
- Unverified: some files test `WIN32`, others `_WIN32`
  (`net.cpp:4,1380` against `net_upnp.cpp:10`); clang for
  `x86_64-w64-windows-gnu` is expected to define both.

### 2.2 Sockets and the platform layer

The module is a separate DLL and psport's `ps_host_*` live in
`psport.exe`, so the module keeps the inherited `#ifdef _WIN32` branches
(Winsock 2, `afunix.h`, `mstcpip.h`) rather than routing through
`src/platform`. Work left:

- `wepoll` (`net_epoll.h:16`) is not in the tree: vendored per the fork
  vendoring rule (verbatim first commit, listed in the README). Unverified:
  its licence (BSD-2-Clause as far as known) and whether MSYS2 packages it.
- The epoll abort psport added (#731, #735) on Windows: a socket pair added
  to the wepoll set as `p2p_sockets.cpp` already does, so
  `sceNetEpollAbort` wakes waiters there too. Today it is a `LOG_TRACE`
  stub off Linux (`net.cpp:666`), which breaks "nothing silent".
- `sceNetEpollWait`'s Windows branch (`net.cpp:963-965`): the deadline loop
  and `EINTR` handling ported, and the microsecond timeout no longer
  truncated to 0 ms below 1000 us.
- Windows Defender Firewall prompts the first time `psport.exe` listens on
  a non-loopback port (P2P UDP). Nothing decanter can grant without admin
  rights (see decisions).

### 2.3 TLS and certificates

cpp-httplib 0.45.0 over mbedTLS already handles Windows (read in the
vendored `gpu/third_party/httplib/httplib.h`):

- With no CA file set, `load_system_certs` loads the `ROOT` and `CA`
  system stores into mbedTLS's chain (`httplib.h:16121-16140`,
  `17723-17733`).
- On top, `CPPHTTPLIB_WINDOWS_AUTOMATIC_ROOT_CERTIFICATES_UPDATE` (on by
  default on `_WIN32`, `httplib.h:353-358`) checks the server's chain with
  `CertGetCertificateChain` (`httplib.h:9074-9104`, `15964-15980`), which
  also fetches roots Windows has not cached yet. It is skipped when a CA
  file, folder or PEM was given.

So on Windows the bundle step of `run.sh` must not happen: no
`SSL_CERT_FILE`. decanter removes `SSL_CERT_FILE` from `psport.exe`'s
environment on Windows (a value inherited from the user's environment
would replace the system roots and switch the second check off).
`PsNetTls::UseCaBundle` stays as it is. Never call
`enable_server_certificate_verification(false)` or
`enable_windows_certificate_verification(false)`.

### 2.4 `scripts/online.py`

Its three commands: `check` (already Rust, `online.rs:387-487`), `overrides`
and `local-server`. Options for `overrides`:

- A. Rust in decanter, beside the functions `online.rs` already mirrors
  (`community_address`, `webapi_address`): `write_overrides`,
  `summon_service`, written into the user folder before the direct launch.
  Two implementations (Python for `run.sh`, Rust for Windows).
- B. C++ in the module: `psport_net_init` builds the override table in
  memory from the title's servers (passed in `PsNetHost`, ABI 4) and
  `PS_COMMUNITY_SERVER`/`PS_SHADNET_WEBAPI`, used by `http.cpp` when no
  `SHADPS4_HTTP_HOST_OVERRIDES_JSON` is set; `host_overrides.custom.json`
  still wins. One implementation for every launcher and OS, owned where
  the title table lives; `run.sh` drops that Python call.

Recommended: B. The summon probe then runs inside the module at the first
network import, which only happens with `PS_ONLINE=1`.

`local-server`'s decision goes with the server (2.5).

### 2.5 Local game server

- A. Python required on Windows: rejected by the brief.
- B. An embeddable Python shipped with psport: another runtime to update
  and vet.
- C. C++ in Bloodborne's extension (`gpu/games/bloodborne/net/`): a thread
  serving `127.0.0.1:<port>` with cpp-httplib's server (already vendored),
  started by the extension when `PS_BBSC_LOCAL_SERVER` asks, so it ends with
  the game without `run.sh`'s watcher. Per-title code stays in the title's
  folder, as psport's rules ask.
- D. Rust in decanter: wrong owner (title code, psport's protocol).

Recommended: C. Until it lands, Windows offers no local server
(`LocalServer::Off`, its selector hidden).

### 2.6 UPnP

miniupnpc is portable and the call sites have no `#ifdef`. Unverified:
`UPNP_GetValidIGD` changed arity in miniupnpc 2.2.8; the call at
`net_upnp.cpp:82` must match the version the toolchain ships. A mapping is
useless while the firewall blocks the port (2.2).

### 2.7 Credentials

- Store: `keyring` 4.2.0 with `windows-native-keyring-store` 1.1.0 is
  already in `Cargo.lock`, selected by default on Windows
  (`crates/core/Cargo.toml:19-26`); GOG sessions already use it there
  (`docs/windows-support.md:15-16`). `credentials.rs` needs no change.
- Size: a password is stored as UTF-16 and refused above
  `CRED_MAX_CREDENTIAL_BLOB_SIZE` (2560 bytes; `utils.rs:79-93` of that
  crate); `MAX_SECRET` (1024 UTF-8 bytes, `online.rs:40`) is at most 2048
  bytes in UTF-16, so it fits.
- Persistence: the crate's default is `Enterprise` (`store.rs:121`), which
  roams with a domain roaming profile; `Local` keeps it on this machine.
- Hand-over: psport reads the secrets only from the environment
  (`user_manager.h:41-43`). On Windows a process of the same user can read
  another's environment, the exposure #716 notes for Linux. A handle
  (an inherited pipe the module reads once) removes it on both systems.

### 2.8 Title data and features on Windows

- `title_online` (`online.rs:343-382`) gains the direct form
  `psport.exe --online-overrides --title ID`, as `Lister::Direct` does for
  mods (`crates/ps/src/bbport/tools.rs:172`).
- decanter does not ask `psport.exe --list-features` on Windows yet
  (`docs/windows-support.md:224-226`; `features.rs:211-235` is
  toolchain-only). `PsOnline` follows that report, so the direct query is
  a prerequisite.

## 3. Phases

Each phase lands on its own and leaves Windows offline-safe.

| # | Repo | What | Files | Model | Verified by (no online) |
|---|---|---|---|---|---|
| 0 | board | Owner's decisions (section 4) | kb | owner | answers on #1296 |
| 1 | psport | Module builds for Windows, not shipped | `CMakeLists.txt:35`, `gpu/CMakeLists.txt:284-312`, `gpu/net/CMakeLists.txt`, `.github/workflows/windows.yml` packages, `tools/win32-cross.sh:240`, committed `shadnet.pb.*`, vendored wepoll, `.def` | opus (C++ link and ABI on a new platform) | `tools/win32-cross.sh build`: `libpsport-net.dll` links; `llvm-objdump -p`: three exports, imports without OpenSSL, Python or msvcrt; Linux build and `readelf -d` as in #850; the Windows CI job green |
| 2 | psport | Runtime loads it on Windows | `runtime_net.c:65-73`, `src/platform/win32` (module name), `host_features.inc` (`online_module` `WIN32 EXPERIMENTAL`), `net_epoll.h`, `net.cpp:648-669,911-966` | opus (sockets, concurrency) | `psport.exe --list-features` lists `online_module` with and without the DLL; an OpenOrbis homebrew (`tools/hwtest`) doing `sceNet` sockets, epoll, abort and timeouts on `127.0.0.1` with `PS_ONLINE=1` and `PS_SHADNET_SERVER=127.0.0.1:<closed port>`, run by the Windows agent; same homebrew on Linux unchanged |
| 3 | psport | TLS on Windows | `gpu/net/CMakeLists.txt` (`crypt32`), nothing in `psport_net_tls.h` | opus (TLS trust) | #850's local handshake set against a server on `127.0.0.1` with a test CA: refused while the CA is untrusted, accepted once it is in the test machine's CurrentUser `ROOT`; `SSL_CERT_FILE` unset in the log |
| 4 | psport | Host overrides inside the module (option B) | `psport_net.h` (ABI 4), `runtime_net.c:106-111`, `psport_net_glue.cpp`, `http.cpp:298-318`, `run.sh:328`, `scripts/online.py` | opus (C/C++ ABI change) | Linux: same JSON as `online.py overrides` for the same inputs, with a stub WebAPI on `127.0.0.1` answering 400 and then 404; log line `loaded N host override entries` |
| 5 | decanter | Direct launch online | `crates/core/src/platform/windows.rs:219` (`Testing`), `features.rs` and `online.rs` direct queries, `direct.rs` (`SSL_CERT_FILE` removed), `LocalServer` held `Off` on Windows, `winbuild/assemble.rs` (copies the DLL when present), `pins.rs`, `docs/windows-support.md` | sonnet (Rust wiring under clippy); opus if it fails | `cargo clippy` native and `x86_64-pc-windows-gnu`, `cargo fmt --check`; Windows agent: Online tab shown with the testing badge only with a build listing `online_module`, a launch with every server at `127.0.0.1` logs `Online: module ...libpsport-net.dll`, `SSL_CERT_FILE` absent, secrets not in the run log |
| 6 | psport | Local game server in C++ | `gpu/games/bloodborne/net/` (server), `title.cpp:136-138`, `run.sh:309-326`, `scripts/bbsc_gameserver/` retired | opus (protocol fidelity) | Linux: the Python and C++ servers on `127.0.0.1` answer a recorded request set byte for byte; listens on loopback only (`ss -ltn`, `netstat -an` on Windows) |
| 7 | decanter | Local server on Windows | `online.rs` (`LocalServer` unlocked), the Bloodborne tab | sonnet | clippy; Windows agent sees the selector; loopback launch logs the server's start |
| 8 | both | Secrets through a handle (#716) | `user_manager.h`, `online.rs:286-308`, `direct.rs`, `run.sh` | opus (security) | the secrets are absent from the process environment (`/proc/<pid>/environ` on Linux; a dump of the environment block on Windows) while a loopback login reaches a stub on `127.0.0.1` |
| 9 | psport | Ship the module in the Windows package | `tools/win32-bundle.sh:185-187`, `tools/check_bundle_licences.sh`, CI | sonnet | licence check green; the owner plays online and reports |

## 4. Risks and open decisions

Risks:

- TLS trust: any `SSL_CERT_FILE` on Windows replaces the system roots and
  disables the `CertGetCertificateChain` check (`httplib.h:15968-15970`).
  Never set it, never turn verification off, and keep
  `host_overrides.custom.json` from turning an `https` endpoint into
  `http` silently (it already can on Linux: logged, not refused).
- cpp-httplib loads the `CA` (intermediate) store as trust anchors too
  (`httplib.h:16125`); the Schannel check bounds it while it stays on.
- shadNet login travels in clear over TCP (`client.cpp:206,303`): the same
  on Linux, not introduced here, but the password is real.
- Secrets in the environment (#716), readable by any process of the user.
- `PS_NET_MODULE` loads any DLL path; decanter never sets it from settings.
  `LoadLibraryExW` resolves the module's dependencies from its folder first,
  so that folder must not be writable by others (decanter checks local
  drive and links, `direct.rs:47-78`, not ACLs).
- Paths: `psport.exe` carries no UTF-8 `activeCodePage` manifest, and the
  module builds `std::filesystem::path` from `char*`
  (`http.cpp:300-306`, the `npbind.dat` path in `psport_net_glue.cpp:420`).
  Unverified whether libc++ then reads them in the ANSI code page; a user
  folder outside it would break.
- Agents can only check loopback; the first real test is the owner's.

Decisions for the owner:

1. Ship `libpsport-net.dll` in the Windows package (CI and decanter's own
   build), or keep it local-only as #851 does for the Linux bundle.
2. Where `online.py overrides` goes: the module (B, recommended) or Rust in
   decanter (A).
3. The local game server on Windows: C++ in Bloodborne's extension (C,
   recommended), none on Windows, or an embedded Python.
4. Credential Manager persistence `Local` or the crate's `Enterprise`, and
   whether the secrets move from the environment to a handle (phase 8)
   before Windows online is offered.
5. Firewall: let Windows prompt (recommended), or decanter add a rule,
   which needs elevation.
6. Generated shadNet messages committed, or Python in the toolchain package.

## 5. decanter's capabilities and UI per phase

| After | `PsOnline` on Windows | UI |
|---|---|---|
| today | `Unavailable(NotOnThisSystem)` (`windows.rs:219`) | Online tab and Ajustes' section hidden (`detail.rs:623`, `settings.rs:394`) |
| 1-4 | unchanged | unchanged: psport has the module but decanter neither asks nor launches it |
| 5 | `Testing` in `windows.rs`; restricted to `NeedsSetup` until the direct `--list-features` lists `online_module` (`app/src/capabilities.rs:163-168`), experimental rows keep the badge | Online tab and Ajustes' section shown with the testing badge (`gate_or_reason`, `games/online.rs:229,300`, `views/ps/online.rs:325-331`); BBSC switch shown, local server selector hidden; Probar conexión works as on Linux; `docs/windows-support.md:144-151,176` rewritten |
| 7 | unchanged | local server selector shown |
| 9 and the owner's test | `Available` | badge removed |
