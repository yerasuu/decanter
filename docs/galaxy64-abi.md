# `Galaxy64.dll` ABI map

Derived from the binary rather than from headers: GOG does not publish
the Galaxy SDK without a registered developer account. Everything below
comes from parsing the PE, walking its RTTI, and reading the logging
strings each method passes carrying its own name.

Regenerate with `python3 -I tools/gen_abi_doc.py <path-to-Galaxy64.dll> docs/galaxy64-abi.md`.
The binary itself is **not** in this repo (15 MB, proprietary to GOG): copy it out
of an installed game. The build this file was generated from is identified by
its SHA-256 below; a different build gives different addresses, and the generator
finds every helper by the strings it references rather than by address.
[`galaxy64-api-notes.md`](galaxy64-api-notes.md) is a hand-written companion (argument types, return
values, preconditions of the main facades); it is **not** regenerated.

## What is proven and what is inferred

Everything is read from the binary, in three tiers:

- **Proven from the binary.** The interface of each vtable (RTTI), the slot
  class (see below), and a method name read from the code of that very slot.
  Every implemented facade method has ten catch handlers; one of them logs
  `<Method>: arg=%fmt, ...`, which gives the method name, the argument names
  and order, and each argument's printf type. That log string names 281 of the
  298 implemented slots; 8 more take the string the body passes to a
  precondition helper, which is the method name too. The *Errors* column comes
  from the precondition helpers a body calls, found by their message strings
  (`%s: not logged on`, `'%s' is null`, ...).
- **Proven, with a caveat.** GOG copy-pasted some catch-handler log strings, so
  two slots of one vtable log the same name. When the body's own string differs
  it settles the slot (1 slot here, with a `Basis` that says so).
- **Inferred**, marked with `†` and a `Basis` saying why: a stub or `_purecall`
  slot has no code to read a name from, so it takes the name of the same slot in
  a sibling vtable for the same interface; a copy-pasted log string with no
  distinguishing body string is assumed to be the `Copy` variant when it has a
  `buffer` argument (the `†` rows of implemented slots).

Argument types are not decoded: the *Arguments* column is the logged text
verbatim, so `%llu` is a 64-bit value and `%s` a C string, and trailing listener or
out parameters that the log omits are not listed.

Slot classes: `thunk` (destructor adjustor thunk, `sub rcx, N; jmp`, slot 0 of
facade vtables), `destructor` (plain scalar deleting destructor), `purecall`
(resolves to the CRT function that decodes the purecall handler and calls it),
`stub` (references "Specified method is not implemented on current facade."
and stores an InvalidState error at TLS+0x110 without throwing), `method`
(implemented). The implemented slots left unnamed are the four error `GetType`
functions (they return a constant) and two listener-registrar methods.

## The binary

| | |
|---|---|
| Format | PE32+ (x86-64), DLL |
| Image base | `0x180000000` |
| Sections | 11 |
| Exports | 28 |
| vtables carrying RTTI | 2550 |
| `galaxy::api` interfaces | 27 |
| SHA-256 | `cdc1dd81e6287b169b0921c5011add80bcd5512d0b9e5f977e178ef16d0d924a` |
| `.text` virtual size | `0x9b4f87` |

### Sections

| Section | VMA | Virtual size | File offset |
|---|---|---|---|
| `.text` | `0x180001000` | `0x9b4f87` | `0x400` |
| `.rdata` | `0x1809b6000` | `0x3755b4` | `0x9b5400` |
| `.data` | `0x180d2c000` | `0xac4e1` | `0xd2aa00` |
| `.pdata` | `0x180dd9000` | `0xa1124` | `0xdc4c00` |
| `.idata` | `0x180e7b000` | `0x3940` | `0xe65e00` |
| `.tls` | `0x180e7f000` | `0x11ac` | `0xe69800` |
| `.gfids` | `0x180e81000` | `0x1c6f` | `0xe6aa00` |
| `.00cfg` | `0x180e83000` | `0x11b` | `0xe6c800` |
| `_RDATA` | `0x180e84000` | `0x999c` | `0xe6ca00` |
| `.rsrc` | `0x180e8e000` | `0x78c` | `0xe76400` |
| `.reloc` | `0x180e8f000` | `0x1a593` | `0xe76c00` |

## Exports

The 28 symbols a game links against. Most are no-argument accessors
returning a pointer to a singleton interface, and that is where a proxy
DLL gets its opening: the accessor is the only moment a real interface
pointer is handed over, so it is the natural place to patch a vtable.

Each accessor compiles to the same shape — load a global, return it
offset by the interface's position within the implementing object:

```asm
mov  rax, [rip+singleton]   ; the facade instance
test rax, rax
je   .null
add  rax, 0x58              ; offset of the interface subobject
ret
```

| # | Symbol | Resolved | Returns |
|---|---|---|---|
| 1 | `?Apps@api@galaxy@@YAPEAVIApps@12@XZ` | `0x1800c1440` | singleton `0x180dc61c8` + `0x58` |
| 2 | `?Chat@api@galaxy@@YAPEAVIChat@12@XZ` | `0x1800c1460` | singleton `0x180dc6198` + `0x58` |
| 3 | `?CustomNetworking@api@galaxy@@YAPEAVICustomNetworking@12@XZ` | `0x1800c16c0` | singleton `0x180dc61d0` + `0x58` |
| 4 | `?Friends@api@galaxy@@YAPEAVIFriends@12@XZ` | `0x1800c16e0` | singleton `0x180dc6190` + `0x58` |
| 5 | `?GameServerListenerRegistrar@api@galaxy@@YAPEAVIListenerRegistrar@12@XZ` | `0x1800cb550` | — |
| 6 | `?GameServerLogger@api@galaxy@@YAPEAVILogger@12@XZ` | `0x1800cb660` | singleton `0x180dc6320` + `0x58` |
| 7 | `?GameServerMatchmaking@api@galaxy@@YAPEAVIMatchmaking@12@XZ` | `0x1800cb680` | singleton `0x180dc6300` + `0x58` |
| 8 | `?GameServerNetworking@api@galaxy@@YAPEAVINetworking@12@XZ` | `0x1800cb6a0` | singleton `0x180dc6308` + `0x58` |
| 9 | `?GameServerTelemetry@api@galaxy@@YAPEAVITelemetry@12@XZ` | `0x1800cb6c0` | singleton `0x180dc6318` + `0x58` |
| 10 | `?GameServerUser@api@galaxy@@YAPEAVIUser@12@XZ` | `0x1800cb6e0` | singleton `0x180dc62f8` + `0x58` |
| 11 | `?GameServerUtils@api@galaxy@@YAPEAVIUtils@12@XZ` | `0x1800cb700` | singleton `0x180dc6310` + `0x58` |
| 12 | `?GetError@api@galaxy@@YAPEBVIError@12@XZ` | `0x180143520` | — |
| 13 | `?Init@api@galaxy@@YAXAEBUInitOptions@12@@Z` | `0x1800c1950` | `GalaxyFacade` |
| 14 | `?InitGameServer@api@galaxy@@YAXAEBUInitOptions@12@@Z` | `0x1800cb740` | `GalaxyGameServerFacade` |
| 15 | `?ListenerRegistrar@api@galaxy@@YAPEAVIListenerRegistrar@12@XZ` | `0x1800c23b0` | — |
| 16 | `?Logger@api@galaxy@@YAPEAVILogger@12@XZ` | `0x1800c26a0` | singleton `0x180dc61d8` + `0x58` |
| 17 | `?Matchmaking@api@galaxy@@YAPEAVIMatchmaking@12@XZ` | `0x1800c26c0` | singleton `0x180dc61a0` + `0x58` |
| 18 | `?Networking@api@galaxy@@YAPEAVINetworking@12@XZ` | `0x1800c26e0` | singleton `0x180dc61a8` + `0x58` |
| 19 | `?ProcessData@api@galaxy@@YAXXZ` | `0x1800c2700` | — |
| 20 | `?ProcessGameServerData@api@galaxy@@YAXXZ` | `0x1800cc140` | — |
| 21 | `?Shutdown@api@galaxy@@YAXXZ` | `0x1800c29e0` | — |
| 22 | `?ShutdownGameServer@api@galaxy@@YAXXZ` | `0x1800cc3c0` | — |
| 23 | `?Stats@api@galaxy@@YAPEAVIStats@12@XZ` | `0x1800c2a30` | singleton `0x180dc61b0` + `0x58` |
| 24 | `?Storage@api@galaxy@@YAPEAVIStorage@12@XZ` | `0x1800c2a50` | singleton `0x180dc61c0` + `0x58` |
| 25 | `?Telemetry@api@galaxy@@YAPEAVITelemetry@12@XZ` | `0x1800c2a70` | singleton `0x180dc61e0` + `0x58` |
| 26 | `?User@api@galaxy@@YAPEAVIUser@12@XZ` | `0x1800c42a0` | singleton `0x180dc6188` + `0x58` |
| 27 | `?Utils@api@galaxy@@YAPEAVIUtils@12@XZ` | `0x1800c42c0` | singleton `0x180dc61b8` + `0x58` |
| 28 | `load` | `0x1800bc4b0` | — |

`load` is an unmangled export whose body is a bare `ret` — a no-op kept
for export-table compatibility.

## Interfaces

Every row comes from RTTI: a vtable's `CompleteObjectLocator` names the
concrete class, and its `ClassHierarchyDescriptor` says which interface
that particular subobject implements, by matching each base's `mdisp`
against the locator's own subobject offset.

Interfaces listed more than once have several implementations (a peer
one and a game-server one, plus the shared base they derive from).

| Interface | Implemented by | vtable | Subobject offset | Slots |
|---|---|---|---|---|
| `galaxy::api::IApps` | `galaxy::peer::facade::AppsFacade` | `0x1809cd130` | `0x58` | 4 |
| `galaxy::api::IChat` | `galaxy::peer::facade::ChatFacade` | `0x1809c6578` | `0x58` | 9 |
| `galaxy::api::ICustomNetworking` | `galaxy::peer::facade::CustomNetworkingFacade` | `0x1809ccc68` | `0x58` | 8 |
| `galaxy::api::IError` | `galaxy::api::IUnauthorizedAccessError` | `0x1809bd000` | `0x0` | 4 |
| `galaxy::api::IError` | `galaxy::api::IInvalidArgumentError` | `0x1809bd030` | `0x0` | 4 |
| `galaxy::api::IError` | `galaxy::api::IInvalidStateError` | `0x1809bd060` | `0x0` | 4 |
| `galaxy::api::IError` | `galaxy::api::IRuntimeError` | `0x1809bd090` | `0x0` | 4 |
| `galaxy::api::IFriends` | `galaxy::peer::facade::FriendsFacade` | `0x1809c4f60` | `0x58` | 39 |
| `galaxy::api::IInvalidArgumentError` | `galaxy::peer::facade::$00::galaxy::api::VIInvalidArgumentError::?$TypeAwareFacadeError` | `0x1809bd8a8` | `0x0` | 4 |
| `galaxy::api::IInvalidStateError` | `galaxy::peer::facade::$01::galaxy::api::VIInvalidStateError::?$TypeAwareFacadeError` | `0x1809bd988` | `0x0` | 4 |
| `galaxy::api::IListenerRegistrar` | `galaxy::peer::facade::FacadeListenerRegistrar` | `0x1809c1358` | `0x58` | 3 |
| `galaxy::api::ILogger` | `galaxy::peer::facade::LoggerFacade` | `0x1809cd398` | `0x58` | 7 |
| `galaxy::api::IMatchmaking` | `galaxy::peer::facade::PeerMatchmakingFacade` | `0x1809c76b8` | `0x58` | 34 |
| `galaxy::api::IMatchmaking` | `galaxy::peer::facade::GameServerMatchmakingFacade` | `0x1809d02b8` | `0x58` | 34 |
| `galaxy::api::IMatchmaking` | `galaxy::peer::facade::MatchmakingFacadeBase` | `0x1809e1dd8` | `0x58` | 34 |
| `galaxy::api::INetworking` | `galaxy::peer::facade::NetworkingFacade` | `0x1809c8548` | `0x58` | 10 |
| `galaxy::api::IRuntimeError` | `galaxy::peer::facade::$02::galaxy::api::VIRuntimeError::?$TypeAwareFacadeError` | `0x1809bda68` | `0x0` | 4 |
| `galaxy::api::IStats` | `galaxy::peer::facade::StatsFacade` | `0x1809c9e08` | `0x58` | 35 |
| `galaxy::api::IStorage` | `galaxy::peer::facade::StorageFacade` | `0x1809cc238` | `0x58` | 20 |
| `galaxy::api::ITelemetry` | `galaxy::peer::facade::TelemetryFacade` | `0x1809ce0b8` | `0x58` | 15 |
| `galaxy::api::IUnauthorizedAccessError` | `galaxy::peer::facade::$0A::galaxy::api::VIUnauthorizedAccessError::?$TypeAwareFacadeError` | `0x1809bd7c0` | `0x0` | 4 |
| `galaxy::api::IUser` | `galaxy::peer::facade::PeerUserFacade` | `0x1809c2b30` | `0x58` | 31 |
| `galaxy::api::IUser` | `galaxy::peer::facade::GameServerUserFacade` | `0x1809cfe80` | `0x58` | 31 |
| `galaxy::api::IUser` | `galaxy::peer::facade::UserFacadeBase` | `0x1809d15b0` | `0x58` | 31 |
| `galaxy::api::IUtils` | `galaxy::peer::facade::PeerUtilsFacade` | `0x1809cb7b0` | `0x58` | 10 |
| `galaxy::api::IUtils` | `galaxy::peer::facade::GameServerUtilsFacade` | `0x1809d0630` | `0x58` | 10 |
| `galaxy::api::IUtils` | `galaxy::peer::facade::UtilsFacadeBase` | `0x1809e3988` | `0x58` | 10 |

## Method tables

Slot 0 is always the virtual destructor: an *adjustor thunk*
(`sub rcx, <offset>; jmp ...`) whenever the interface does not sit at
offset 0 of the complete object, a plain destructor for the error classes.
`_purecall` slots are the pure virtuals of the interface that the concrete class
does not override. A `stub` is a method a facade deliberately does not implement
(the game-server facades reject most peer calls this way). A name with `†` is
inferred, see above. The *Errors* column lists the error classes the body can
raise through precondition helpers; a blank cell means none were found.

Addresses are the real function bodies, with incremental-link `jmp`
thunks already followed through.

### `galaxy::api::IApps` — `galaxy::peer::facade::AppsFacade`

vtable `0x1809cd130`, 4 slots (3 method, 1 thunk); 3 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x18012c334` | thunk |  |  |  |  |
| 1 | `0x18012c660` | method | `IsDlcInstalled` | `productID=%llu` |  | catch-handler log |
| 2 | `0x18012c3d0` | method | `GetCurrentGameLanguage` | `productID=%llu` |  | catch-handler log |
| 3 | `0x18012c4f0` | method | `GetCurrentGameLanguageCopy` | `productID=%llu` | InvalidArgument (argument null) | catch-handler log |

### `galaxy::api::IChat` — `galaxy::peer::facade::ChatFacade`

vtable `0x1809c6578`, 9 slots (8 method, 1 thunk); 8 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x18010c350` | thunk |  |  |  |  |
| 1 | `0x18010d6b0` | method | `RequestChatRoomWithUser` | `userID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 2 | `0x18010d4b0` | method | `RequestChatRoomMessages` | `chatRoomID=%llu, limit=%u, referenceMessageID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 3 | `0x18010d7e0` | method | `SendChatRoomMessage` | `chatRoomID=%llu, message=(%llu bytes)` | UnauthorizedAccess (not logged on); InvalidArgument (argument null, argument too large) | catch-handler log |
| 4 | `0x18010cba0` | method | `GetChatRoomMessageByIndex` | `index=%u, buffer=%p, bufferLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |
| 5 | `0x18010c740` | method | `GetChatRoomMemberCount` | `chatRoomID=%llu, index=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 6 | `0x18010c930` | method | `GetChatRoomMemberUserIDByIndex` | `chatRoomID=%llu, index=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 7 | `0x18010cea0` | method | `GetChatRoomUnreadMessageCount` | `chatRoomID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 8 | `0x18010d200` | method | `MarkChatRoomAsRead` | `chatRoomID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |

### `galaxy::api::ICustomNetworking` — `galaxy::peer::facade::CustomNetworkingFacade`

vtable `0x1809ccc68`, 8 slots (7 method, 1 thunk); 7 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x18012ad60` | thunk |  |  |  |  |
| 1 | `0x18012b170` | method | `OpenConnection` | `connectionString=%s` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 2 | `0x18012af20` | method | `CloseConnection` | `connectionID=%llu` | UnauthorizedAccess (not signed in) | catch-handler log |
| 3 | `0x18012bc60` | method | `SendData` | `connectionID=%llu, data=%p, dataSize=%u` | UnauthorizedAccess (not signed in); InvalidArgument (argument null) | catch-handler log |
| 4 | `0x18012b040` | method | `GetAvailableDataSize` | `connectionID=%llu` | UnauthorizedAccess (not signed in) | catch-handler log |
| 5 | `0x18012b590` | method | `PeekData` | `connectionID=%llu, dataSize=%u` | UnauthorizedAccess (not signed in); InvalidArgument (argument null) | catch-handler log |
| 6 | `0x18012bac0` | method | `ReadData` | `connectionID=%llu, dataSize=%u` | UnauthorizedAccess (not signed in); InvalidArgument (argument null) | catch-handler log |
| 7 | `0x18012b720` | method | `PopData` | `connectionID=%llu, dataSize=%u` | UnauthorizedAccess (not signed in) | catch-handler log |

### `galaxy::api::IError` — `galaxy::api::IUnauthorizedAccessError`

vtable `0x1809bd000`, 4 slots (1 destructor, 3 purecall); 0 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1800c0ea0` | destructor |  |  |  |  |
| 1 | `0x18066286c` | purecall |  |  |  |  |
| 2 | `0x18066286c` | purecall |  |  |  |  |
| 3 | `0x18066286c` | purecall |  |  |  |  |

### `galaxy::api::IError` — `galaxy::api::IInvalidArgumentError`

vtable `0x1809bd030`, 4 slots (1 destructor, 3 purecall); 0 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1800c0ce0` | destructor |  |  |  |  |
| 1 | `0x18066286c` | purecall |  |  |  |  |
| 2 | `0x18066286c` | purecall |  |  |  |  |
| 3 | `0x18066286c` | purecall |  |  |  |  |

### `galaxy::api::IError` — `galaxy::api::IInvalidStateError`

vtable `0x1809bd060`, 4 slots (1 destructor, 3 purecall); 0 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1800c0d60` | destructor |  |  |  |  |
| 1 | `0x18066286c` | purecall |  |  |  |  |
| 2 | `0x18066286c` | purecall |  |  |  |  |
| 3 | `0x18066286c` | purecall |  |  |  |  |

### `galaxy::api::IError` — `galaxy::api::IRuntimeError`

vtable `0x1809bd090`, 4 slots (1 destructor, 3 purecall); 0 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1800c0e20` | destructor |  |  |  |  |
| 1 | `0x18066286c` | purecall |  |  |  |  |
| 2 | `0x18066286c` | purecall |  |  |  |  |
| 3 | `0x18066286c` | purecall |  |  |  |  |

### `galaxy::api::IFriends` — `galaxy::peer::facade::FriendsFacade`

vtable `0x1809c4f60`, 39 slots (38 method, 1 thunk); 38 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1801032cc` | thunk |  |  |  |  |
| 1 | `0x180104130` | method | `GetDefaultAvatarCriteria` |  |  | catch-handler log |
| 2 | `0x180107f10` | method | `SetDefaultAvatarCriteria` | `defaultAvatarCriteria=%u` |  | catch-handler log |
| 3 | `0x180107840` | method | `RequestUserInformation` | `userID=%llu, avatarCriteria=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 4 | `0x1801065b0` | method | `IsUserInformationAvailable` | `userID=%llu` |  | catch-handler log |
| 5 | `0x1801052c0` | method | `GetPersonaName` |  | UnauthorizedAccess (not signed in) | catch-handler log |
| 6 | `0x1801053f0` | method | `GetPersonaNameCopy` | `buffer=%p, bufferLength=%u` | UnauthorizedAccess (not signed in); InvalidArgument (argument null) | catch-handler log |
| 7 | `0x180105540` | method | `GetPersonaState` |  |  | catch-handler log |
| 8 | `0x180104de0` | method | `GetFriendPersonaName` | `userID=%llu` |  | catch-handler log |
| 9 | `0x180104f40` | method | `GetFriendPersonaNameCopy` | `userID=%llu, buffer=%p, bufferLength=%u` | InvalidArgument (argument null) | catch-handler log |
| 10 | `0x1801050a0` | method | `GetFriendPersonaState` | `userID=%llu` |  | catch-handler log |
| 11 | `0x1801044f0` | method | `GetFriendAvatarUrl` | `userID=%llu, avatarType=%d` |  | catch-handler log |
| 12 | `0x180104670` | method | `GetFriendAvatarUrlCopy`† | `userID=%llu, avatarType=%d, buffer=%p, bufferLength=%u` | InvalidArgument (argument null) | log string is a copy-paste of the previous slot; `buffer` argument marks the Copy variant |
| 13 | `0x180104230` | method | `GetFriendAvatarImageID` | `userID=%llu, avatarType=%u` |  | catch-handler log |
| 14 | `0x180104380` | method | `GetFriendAvatarImageRGBA` | `userID=%llu, avatarType=%u, buffer=%p, bufferLength=%u` | InvalidArgument (argument null) | catch-handler log |
| 15 | `0x180106280` | method | `IsFriendAvatarImageRGBAAvailable` | `userID=%llu, avatarType=%u` |  | catch-handler log |
| 16 | `0x180107250` | method | `RequestFriendList` |  | UnauthorizedAccess (not logged on) | catch-handler log |
| 17 | `0x180106100` | method | `IsFriend` | `userID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 18 | `0x1801049c0` | method | `GetFriendCount` |  | UnauthorizedAccess (not logged on) | catch-handler log |
| 19 | `0x1801047f0` | method | `GetFriendByIndex` | `index=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 20 | `0x180107c90` | method | `SendFriendInvitation` | `userID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 21 | `0x180107130` | method | `RequestFriendInvitationList` |  | UnauthorizedAccess (not logged on) | catch-handler log |
| 22 | `0x180107720` | method | `RequestSentFriendInvitationList` |  | UnauthorizedAccess (not logged on) | catch-handler log |
| 23 | `0x180104ca0` | method | `GetFriendInvitationCount` |  | UnauthorizedAccess (not logged on) | catch-handler log |
| 24 | `0x180104ad0` | method | `GetFriendInvitationByIndex` | `index=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 25 | `0x180107b60` | method | `RespondToFriendInvitation` | `userID=%llu, accept=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 26 | `0x180103ac0` | method | `DeleteFriend` | `userID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 27 | `0x180108100` | method | `SetRichPresence` | `key=%s, value=%s` | UnauthorizedAccess (not logged on, not signed in); InvalidState (not signed in with GOG); InvalidArgument (argument null, argument too large) | catch-handler log |
| 28 | `0x180103bf0` | method | `DeleteRichPresence` | `key=%s` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |
| 29 | `0x1801037d0` | method | `ClearRichPresence` |  | UnauthorizedAccess (not logged on) | catch-handler log |
| 30 | `0x1801074a0` | method | `RequestRichPresence` | `userID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 31 | `0x180105640` | method | `GetRichPresence` | `userID=%llu, key=%s` | InvalidArgument (argument null) | catch-handler log |
| 32 | `0x180105c00` | method | `GetRichPresenceCopy` | `userID=%llu, key=%s, buffer=%p, bufferLength=%u` | InvalidArgument (argument null) | catch-handler log |
| 33 | `0x180105f10` | method | `GetRichPresenceCount` | `userID=%llu` |  | catch-handler log |
| 34 | `0x180105940` | method | `GetRichPresenceByIndex` | `userID=%llu, index=%u` | InvalidArgument (argument null) | catch-handler log |
| 35 | `0x1801086a0` | method | `ShowOverlayInviteDialog` | `connectionString=%s` | InvalidState (not signed in with Galaxy); UnauthorizedAccess (not logged on, not signed in); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 36 | `0x180107dc0` | method | `SendInvitation` | `userID=%llu, connectionString=%s` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 37 | `0x180104000` | method | `FindUser` | `userSpecifier=(%u bytes)` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 38 | `0x1801063d0` | method | `IsUserInTheSameGame` | `userID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |

### `galaxy::api::IInvalidArgumentError` — `galaxy::peer::facade::$00::galaxy::api::VIInvalidArgumentError::?$TypeAwareFacadeError`

vtable `0x1809bd8a8`, 4 slots (1 destructor, 1 method, 2 purecall); 0 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1800c0660` | destructor |  |  |  |  |
| 1 | `0x18066286c` | purecall |  |  |  |  |
| 2 | `0x18066286c` | purecall |  |  |  |  |
| 3 | `0x1800c1910` | method | *returns constant 1 (InvalidArgument)* |  |  |  |

### `galaxy::api::IInvalidStateError` — `galaxy::peer::facade::$01::galaxy::api::VIInvalidStateError::?$TypeAwareFacadeError`

vtable `0x1809bd988`, 4 slots (1 destructor, 1 method, 2 purecall); 0 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1800c06a0` | destructor |  |  |  |  |
| 1 | `0x18066286c` | purecall |  |  |  |  |
| 2 | `0x18066286c` | purecall |  |  |  |  |
| 3 | `0x1800c1920` | method | *returns constant 2 (InvalidState)* |  |  |  |

### `galaxy::api::IListenerRegistrar` — `galaxy::peer::facade::FacadeListenerRegistrar`

vtable `0x1809c1358`, 3 slots (2 method, 1 thunk); 0 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1800dd460` | thunk |  |  |  |  |
| 1 | `0x1800e1590` | method |  |  |  |  |
| 2 | `0x1800e2cf0` | method |  |  |  |  |

### `galaxy::api::ILogger` — `galaxy::peer::facade::LoggerFacade`

vtable `0x1809cd398`, 7 slots (6 method, 1 thunk); 6 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x18012c890` | thunk |  |  |  |  |
| 1 | `0x18012d010` | method | `Trace` | `format=%s` | InvalidArgument (argument null) | catch-handler log |
| 2 | `0x18012c990` | method | `Debug` | `format=%s` | InvalidArgument (argument null) | catch-handler log |
| 3 | `0x18012ce70` | method | `Info` | `format=%s` | InvalidArgument (argument null) | catch-handler log |
| 4 | `0x18012d1a0` | method | `Warning` | `format=%s` | InvalidArgument (argument null) | catch-handler log |
| 5 | `0x18012cb30` | method | `Error` | `format=%s` | InvalidArgument (argument null) | catch-handler log |
| 6 | `0x18012ccd0` | method | `Fatal` | `format=%s` | InvalidArgument (argument null) | catch-handler log |

### `galaxy::api::IMatchmaking` — `galaxy::peer::facade::PeerMatchmakingFacade`

vtable `0x1809c76b8`, 34 slots (33 method, 1 thunk); 33 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x180111794` | thunk |  |  |  |  |
| 1 | `0x1801ecec0` | method | `CreateLobby` | `lobbyType=%d, maxMembers=%u, joinable=%u, lobbyTopologyType=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 2 | `0x180113c30` | method | `RequestLobbyList` | `allowFullLobbies=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 3 | `0x180111f30` | method | `AddRequestLobbyListResultCountFilter` | `limit=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 4 | `0x180112080` | method | `AddRequestLobbyListStringFilter` | `keyToMatch=%s, valueToMatch=%s, comparisonType=%d` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null) | catch-handler log |
| 5 | `0x180111c70` | method | `AddRequestLobbyListNumericalFilter` | `keyToMatch=%s, valueToMatch=%d, comparisonType=%d` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null) | catch-handler log |
| 6 | `0x180111a30` | method | `AddRequestLobbyListNearValueFilter` | `keyToMatch=%s, valueToBeCloseTo=%d` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null) | catch-handler log |
| 7 | `0x180112510` | method | `GetLobbyByIndex` | `lobbyIndex=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 8 | `0x1801130b0` | method | `JoinLobby` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 9 | `0x1801f0f50` | method | `LeaveLobby` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 10 | `0x1801f30d0` | method | `SetMaxNumLobbyMembers` | `lobbyID=%llu, maxNumLobbyMembers=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 11 | `0x1801efeb0` | method | `GetMaxNumLobbyMembers` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 12 | `0x1801f00f0` | method | `GetNumLobbyMembers` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 13 | `0x1801ee0b0` | method | `GetLobbyMemberByIndex` | `lobbyID=%llu, index=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 14 | `0x1801f29d0` | method | `SetLobbyType` | `lobbyID=%llu, lobbyType=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 15 | `0x1801efc50` | method | `GetLobbyType` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 16 | `0x1801f2330` | method | `SetLobbyJoinable` | `lobbyID=%llu, joinable=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 17 | `0x1801f0d10` | method | `IsLobbyJoinable` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 18 | `0x180113570` | method | `RequestLobbyData` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 19 | `0x1801ed380` | method | `GetLobbyData` | `lobbyID=%llu, key=%s` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 20 | `0x1801eda40` | method | `GetLobbyDataCopy` | `lobbyID=%llu, key=%s, buffer=%p, bufferLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 21 | `0x1801f22e0` | method | `SetLobbyData` |  |  | string passed to a precondition helper |
| 22 | `0x1801ede70` | method | `GetLobbyDataCount` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 23 | `0x1801ed700` | method | `GetLobbyDataByIndex` | `lobbyID=%llu, index=%u, key=%p, keyLength=%u, value=%p, valueLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |
| 24 | `0x1801ed330` | method | `DeleteLobbyData` |  |  | string passed to a precondition helper |
| 25 | `0x1801ee410` | method | `GetLobbyMemberData` | `lobbyID=%llu, memberID=%llu, key=%s` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 26 | `0x1801eec50` | method | `GetLobbyMemberDataCopy` | `lobbyID=%llu, memberID=%llu, key=%s, buffer=%p, bufferLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 27 | `0x180113da0` | method | `SetLobbyMemberData` |  |  | string passed to a precondition helper |
| 28 | `0x1801ef230` | method | `GetLobbyMemberDataCount` | `lobbyID=%llu, memberID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 29 | `0x1801ee850` | method | `GetLobbyMemberDataByIndex` | `lobbyID=%llu, memberID=%llu, index=%u, key=%p, keyLength=%u, value=%p, valueLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |
| 30 | `0x180112350` | method | `DeleteLobbyMemberData` |  |  | string passed to a precondition helper |
| 31 | `0x1801ef7f0` | method | `GetLobbyOwner` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 32 | `0x1801f1fc0` | method | `SendLobbyMessage` | `lobbyID=%llu, data=%p, dataSize=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null, argument too large) | catch-handler log |
| 33 | `0x1801ef510` | method | `GetLobbyMessage` | `lobbyID=%llu, messageID=%u, senderID=%llu, msg=%p, msgLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |

### `galaxy::api::IMatchmaking` — `galaxy::peer::facade::GameServerMatchmakingFacade`

vtable `0x1809d02b8`, 34 slots (23 method, 10 stub, 1 thunk); 23 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1801445b0` | thunk |  |  |  |  |
| 1 | `0x180144850` | method | `CreateLobby` | `lobbyType=%d, maxMembers=%u, joinable=%u, lobbyTopologyType=%u` |  | catch-handler log |
| 2 | `0x180144c60` | stub | `RequestLobbyList`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 3 | `0x180144730` | stub | `AddRequestLobbyListResultCountFilter`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 4 | `0x1801447c0` | stub | `AddRequestLobbyListStringFilter`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 5 | `0x1801446a0` | stub | `AddRequestLobbyListNumericalFilter`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 6 | `0x180144610` | stub | `AddRequestLobbyListNearValueFilter`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 7 | `0x180144a80` | stub | `GetLobbyByIndex`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 8 | `0x180144b40` | stub | `JoinLobby`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 9 | `0x1801f0f50` | method | `LeaveLobby` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 10 | `0x1801f30d0` | method | `SetMaxNumLobbyMembers` | `lobbyID=%llu, maxNumLobbyMembers=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 11 | `0x1801efeb0` | method | `GetMaxNumLobbyMembers` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 12 | `0x1801f00f0` | method | `GetNumLobbyMembers` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 13 | `0x1801ee0b0` | method | `GetLobbyMemberByIndex` | `lobbyID=%llu, index=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 14 | `0x1801f29d0` | method | `SetLobbyType` | `lobbyID=%llu, lobbyType=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 15 | `0x1801efc50` | method | `GetLobbyType` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 16 | `0x1801f2330` | method | `SetLobbyJoinable` | `lobbyID=%llu, joinable=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 17 | `0x1801f0d10` | method | `IsLobbyJoinable` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 18 | `0x180144bd0` | stub | `RequestLobbyData`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 19 | `0x1801ed380` | method | `GetLobbyData` | `lobbyID=%llu, key=%s` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 20 | `0x1801eda40` | method | `GetLobbyDataCopy` | `lobbyID=%llu, key=%s, buffer=%p, bufferLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 21 | `0x1801f22e0` | method | `SetLobbyData` |  |  | string passed to a precondition helper |
| 22 | `0x1801ede70` | method | `GetLobbyDataCount` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 23 | `0x1801ed700` | method | `GetLobbyDataByIndex` | `lobbyID=%llu, index=%u, key=%p, keyLength=%u, value=%p, valueLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |
| 24 | `0x1801ed330` | method | `DeleteLobbyData` |  |  | string passed to a precondition helper |
| 25 | `0x1801ee410` | method | `GetLobbyMemberData` | `lobbyID=%llu, memberID=%llu, key=%s` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 26 | `0x1801eec50` | method | `GetLobbyMemberDataCopy` | `lobbyID=%llu, memberID=%llu, key=%s, buffer=%p, bufferLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 27 | `0x180144cf0` | stub | `SetLobbyMemberData`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 28 | `0x1801ef230` | method | `GetLobbyMemberDataCount` | `lobbyID=%llu, memberID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 29 | `0x1801ee850` | method | `GetLobbyMemberDataByIndex` | `lobbyID=%llu, memberID=%llu, index=%u, key=%p, keyLength=%u, value=%p, valueLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |
| 30 | `0x1801449f0` | stub | `DeleteLobbyMemberData`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 31 | `0x1801ef7f0` | method | `GetLobbyOwner` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 32 | `0x1801f1fc0` | method | `SendLobbyMessage` | `lobbyID=%llu, data=%p, dataSize=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null, argument too large) | catch-handler log |
| 33 | `0x1801ef510` | method | `GetLobbyMessage` | `lobbyID=%llu, messageID=%u, senderID=%llu, msg=%p, msgLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |

### `galaxy::api::IMatchmaking` — `galaxy::peer::facade::MatchmakingFacadeBase`

vtable `0x1809e1dd8`, 34 slots (23 method, 10 purecall, 1 thunk); 23 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1801ecd20` | thunk |  |  |  |  |
| 1 | `0x1801ecec0` | method | `CreateLobby` | `lobbyType=%d, maxMembers=%u, joinable=%u, lobbyTopologyType=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 2 | `0x18066286c` | purecall | `RequestLobbyList`† |  |  | same slot of a sibling vtable |
| 3 | `0x18066286c` | purecall | `AddRequestLobbyListResultCountFilter`† |  |  | same slot of a sibling vtable |
| 4 | `0x18066286c` | purecall | `AddRequestLobbyListStringFilter`† |  |  | same slot of a sibling vtable |
| 5 | `0x18066286c` | purecall | `AddRequestLobbyListNumericalFilter`† |  |  | same slot of a sibling vtable |
| 6 | `0x18066286c` | purecall | `AddRequestLobbyListNearValueFilter`† |  |  | same slot of a sibling vtable |
| 7 | `0x18066286c` | purecall | `GetLobbyByIndex`† |  |  | same slot of a sibling vtable |
| 8 | `0x18066286c` | purecall | `JoinLobby`† |  |  | same slot of a sibling vtable |
| 9 | `0x1801f0f50` | method | `LeaveLobby` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 10 | `0x1801f30d0` | method | `SetMaxNumLobbyMembers` | `lobbyID=%llu, maxNumLobbyMembers=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 11 | `0x1801efeb0` | method | `GetMaxNumLobbyMembers` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 12 | `0x1801f00f0` | method | `GetNumLobbyMembers` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 13 | `0x1801ee0b0` | method | `GetLobbyMemberByIndex` | `lobbyID=%llu, index=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 14 | `0x1801f29d0` | method | `SetLobbyType` | `lobbyID=%llu, lobbyType=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 15 | `0x1801efc50` | method | `GetLobbyType` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 16 | `0x1801f2330` | method | `SetLobbyJoinable` | `lobbyID=%llu, joinable=%u` | UnauthorizedAccess (not logged on) | catch-handler log |
| 17 | `0x1801f0d10` | method | `IsLobbyJoinable` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 18 | `0x18066286c` | purecall | `RequestLobbyData`† |  |  | same slot of a sibling vtable |
| 19 | `0x1801ed380` | method | `GetLobbyData` | `lobbyID=%llu, key=%s` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 20 | `0x1801eda40` | method | `GetLobbyDataCopy` | `lobbyID=%llu, key=%s, buffer=%p, bufferLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 21 | `0x1801f22e0` | method | `SetLobbyData` |  |  | string passed to a precondition helper |
| 22 | `0x1801ede70` | method | `GetLobbyDataCount` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 23 | `0x1801ed700` | method | `GetLobbyDataByIndex` | `lobbyID=%llu, index=%u, key=%p, keyLength=%u, value=%p, valueLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |
| 24 | `0x1801ed330` | method | `DeleteLobbyData` |  |  | string passed to a precondition helper |
| 25 | `0x1801ee410` | method | `GetLobbyMemberData` | `lobbyID=%llu, memberID=%llu, key=%s` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 26 | `0x1801eec50` | method | `GetLobbyMemberDataCopy` | `lobbyID=%llu, memberID=%llu, key=%s, buffer=%p, bufferLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 27 | `0x18066286c` | purecall | `SetLobbyMemberData`† |  |  | same slot of a sibling vtable |
| 28 | `0x1801ef230` | method | `GetLobbyMemberDataCount` | `lobbyID=%llu, memberID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 29 | `0x1801ee850` | method | `GetLobbyMemberDataByIndex` | `lobbyID=%llu, memberID=%llu, index=%u, key=%p, keyLength=%u, value=%p, valueLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |
| 30 | `0x18066286c` | purecall | `DeleteLobbyMemberData`† |  |  | same slot of a sibling vtable |
| 31 | `0x1801ef7f0` | method | `GetLobbyOwner` | `lobbyID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |
| 32 | `0x1801f1fc0` | method | `SendLobbyMessage` | `lobbyID=%llu, data=%p, dataSize=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null, argument too large) | catch-handler log |
| 33 | `0x1801ef510` | method | `GetLobbyMessage` | `lobbyID=%llu, messageID=%u, senderID=%llu, msg=%p, msgLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |

### `galaxy::api::INetworking` — `galaxy::peer::facade::NetworkingFacade`

vtable `0x1809c8548`, 10 slots (9 method, 1 thunk); 9 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1801169b8` | thunk |  |  |  |  |
| 1 | `0x180118330` | method | `SendP2PPacket` | `remoteID=%llu, data=%p, dataSize=%u, sendType=%u, channel=%u` | UnauthorizedAccess (not signed in); InvalidArgument (argument null) | catch-handler log |
| 2 | `0x180117bb0` | method | `PeekP2PPacket` | `destSize=%u, channel=%u` | UnauthorizedAccess (not signed in); InvalidArgument (argument null) | catch-handler log |
| 3 | `0x180117980` | method | `IsP2PPacketAvailable` | `channel=%u` | UnauthorizedAccess (not signed in) | catch-handler log |
| 4 | `0x180118040` | method | `ReadP2PPacket` | `destSize=%u, channel=%u` | UnauthorizedAccess (not signed in) | catch-handler log |
| 5 | `0x180117e40` | method | `PopP2PPacket` | `channel=%u` | UnauthorizedAccess (not signed in) | catch-handler log |
| 6 | `0x180117230` | method | `GetPingWith` | `galaxyID=%llu` | UnauthorizedAccess (not signed in) | catch-handler log |
| 7 | `0x1801181f0` | method | `RequestNatTypeDetection` |  | UnauthorizedAccess (not signed in) | catch-handler log |
| 8 | `0x180117110` | method | `GetNatType` |  | UnauthorizedAccess (not signed in) | catch-handler log |
| 9 | `0x180116d80` | method | `GetConnectionType` | `userID=%llu` | UnauthorizedAccess (not logged on) | catch-handler log |

### `galaxy::api::IRuntimeError` — `galaxy::peer::facade::$02::galaxy::api::VIRuntimeError::?$TypeAwareFacadeError`

vtable `0x1809bda68`, 4 slots (1 destructor, 1 method, 2 purecall); 0 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1800c06e0` | destructor |  |  |  |  |
| 1 | `0x18066286c` | purecall |  |  |  |  |
| 2 | `0x18066286c` | purecall |  |  |  |  |
| 3 | `0x1800c1930` | method | *returns constant 3 (Runtime)* |  |  |  |

### `galaxy::api::IStats` — `galaxy::peer::facade::StatsFacade`

vtable `0x1809c9e08`, 35 slots (34 method, 1 thunk); 34 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x18011e230` | thunk |  |  |  |  |
| 1 | `0x1801214f0` | method | `RequestUserStatsAndAchievements` | `userID=%llu` | UnauthorizedAccess (not signed in) | catch-handler log |
| 2 | `0x180120460` | method | `GetStatInt` | `name=%s, userID=%llu` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 3 | `0x180120230` | method | `GetStatFloat` | `name=%s, userID=%llu` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 4 | `0x1801223e0` | method | `SetStatInt` | `name=%s, value=%d` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 5 | `0x180122200` | method | `SetStatFloat` | `name=%s, value=%f` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 6 | `0x180123180` | method | `UpdateAvgRateStat` | `name=%s, countThisSession=%f, sessionLength=%g` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 7 | `0x18011eda0` | method | `GetAchievement` | `name=%s, userID=%llu` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 8 | `0x180121b10` | method | `SetAchievement` | `name=%s` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 9 | `0x18011e800` | method | `ClearAchievement` | `name=%s` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 10 | `0x180122650` | method | `StoreStatsAndAchievements` |  | UnauthorizedAccess (not signed in) | catch-handler log |
| 11 | `0x180121950` | method | `ResetStatsAndAchievements` |  | UnauthorizedAccess (not signed in) | catch-handler log |
| 12 | `0x18011f290` | method | `GetAchievementDisplayName` | `name=%s` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 13 | `0x18011f400` | method | `GetAchievementDisplayNameCopy` | `name=%s, buffer=%p, bufferLength=%u` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 14 | `0x18011efb0` | method | `GetAchievementDescription` | `name=%s` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 15 | `0x18011f120` | method | `GetAchievementDescriptionCopy` | `name=%s, buffer=%p, bufferLength=%u` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 16 | `0x180120880` | method | `IsAchievementVisible` | `name=%s` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 17 | `0x1801209e0` | method | `IsAchievementVisibleWhileLocked` | `name=%s` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 18 | `0x1801213d0` | method | `RequestLeaderboards` |  | UnauthorizedAccess (not logged on, not signed in) | catch-handler log |
| 19 | `0x18011f570` | method | `GetLeaderboardDisplayName` | `name=%s` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 20 | `0x18011f6d0` | method | `GetLeaderboardDisplayNameCopy` | `name=%s, buffer=%p, bufferLength=%u` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 21 | `0x18011fc70` | method | `GetLeaderboardSortMethod` | `name=%s` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 22 | `0x18011f830` | method | `GetLeaderboardDisplayType` | `name=%s` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 23 | `0x180121180` | method | `RequestLeaderboardEntriesGlobal` | `name=%s, rangeStart=%u, rangeEnd=%u` | UnauthorizedAccess (not logged on, not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 24 | `0x180120b50` | method | `RequestLeaderboardEntriesAroundUser` | `name=%s, countBefore=%u, countAfter=%u, userID=%llu` | UnauthorizedAccess (not logged on, not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 25 | `0x180120e70` | method | `RequestLeaderboardEntriesForUsers` | `name=%s, userList=%p, userCount=%u` | UnauthorizedAccess (not logged on, not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 26 | `0x18011fe10` | method | `GetRequestedLeaderboardEntry` | `index=%u` | UnauthorizedAccess (not signed in) | catch-handler log |
| 27 | `0x18011ff60` | method | `GetRequestedLeaderboardEntryWithDetails` | `index=%u, details=%p, detailsSize=%u` | UnauthorizedAccess (not signed in) | catch-handler log |
| 28 | `0x180121d30` | method | `SetLeaderboardScore` | `name=%s, score=%d, forceUpdate=%d` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 29 | `0x180121f60` | method | `SetLeaderboardScoreWithDetails` | `name=%s, score=%d, details=%p, destSize=%u, forceUpdate=%d` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 30 | `0x18011f9d0` | method | `GetLeaderboardEntryCount` | `name=%s` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 31 | `0x18011ea00` | method | `FindLeaderboard` | `name=%s` | UnauthorizedAccess (not logged on, not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 32 | `0x18011eb40` | method | `FindOrCreateLeaderboard` | `name=%s, displayName=%s, sortMethod=%u, displayType=%u` | UnauthorizedAccess (not logged on, not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 33 | `0x180121720` | method | `RequestUserTimePlayed` | `userID=%llu` | UnauthorizedAccess (not signed in) | catch-handler log |
| 34 | `0x180120680` | method | `GetUserTimePlayed` | `userID=%llu` | UnauthorizedAccess (not signed in) | catch-handler log |

### `galaxy::api::IStorage` — `galaxy::peer::facade::StorageFacade`

vtable `0x1809cc238`, 20 slots (19 method, 1 thunk); 19 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x180127e20` | thunk |  |  |  |  |
| 1 | `0x1801287b0` | method | `FileWrite` | `fileName=%s, data=%p, dataSize=%u` |  | catch-handler log |
| 2 | `0x1801282c0` | method | `FileRead` | `fileName=%s, data=%p, dataSize=%u` |  | catch-handler log |
| 3 | `0x180128040` | method | `FileDelete` | `fileName=%s` |  | catch-handler log |
| 4 | `0x180128190` | method | `FileExists` | `fileName=%s` | InvalidArgument (argument empty, argument null) | catch-handler log |
| 5 | `0x180128fa0` | method | `GetFileSize` | `fileName=%s` | InvalidArgument (argument empty, argument null) | catch-handler log |
| 6 | `0x180129180` | method | `GetFileTimestamp` | `fileName=%s` | InvalidArgument (argument empty, argument null) | catch-handler log |
| 7 | `0x180128b40` | method | `GetFileCount` |  |  | catch-handler log |
| 8 | `0x180128c40` | method | `GetFileNameByIndex` | `index=%u` |  | catch-handler log |
| 9 | `0x180128d60` | method | `GetFileNameCopyByIndex` | `index=%u, buffer=%p, bufferLength=%u` | InvalidArgument (argument null) | catch-handler log |
| 10 | `0x1801283e0` | method | `FileShare` | `fileName=%s` | UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 11 | `0x180127f40` | method | `DownloadSharedFile` | `sharedFileID=%llu` | UnauthorizedAccess (not signed in) | catch-handler log |
| 12 | `0x1801292b0` | method | `GetSharedFileName` | `sharedFileID=%llu` | UnauthorizedAccess (not signed in) | catch-handler log |
| 13 | `0x180129400` | method | `GetSharedFileNameCopy` | `sharedFileID=%llu, buffer=%p, bufferLength=%u` | UnauthorizedAccess (not signed in); InvalidArgument (argument null) | catch-handler log |
| 14 | `0x180129690` | method | `GetSharedFileSize` | `sharedFileID=%llu` | UnauthorizedAccess (not signed in) | catch-handler log |
| 15 | `0x180129550` | method | `GetSharedFileOwner` | `sharedFileID=%llu` | UnauthorizedAccess (not signed in) | catch-handler log |
| 16 | `0x180129ae0` | method | `SharedFileRead` | `sharedFileID=%llu, data=%p, dataSize=%u, offset=%u` | UnauthorizedAccess (not signed in); InvalidArgument (argument null) | catch-handler log |
| 17 | `0x1801299d0` | method | `SharedFileClose` | `sharedFileID=%llu` | UnauthorizedAccess (not signed in) | catch-handler log |
| 18 | `0x180128a30` | method | `GetDownloadedSharedFileCount` |  | UnauthorizedAccess (not signed in) | catch-handler log |
| 19 | `0x180128910` | method | `GetDownloadedSharedFileByIndex` |  | UnauthorizedAccess (not signed in) | string passed to a precondition helper (the log string is a copy-paste) |

### `galaxy::api::ITelemetry` — `galaxy::peer::facade::TelemetryFacade`

vtable `0x1809ce0b8`, 15 slots (14 method, 1 thunk); 14 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x180130918` | thunk |  |  |  |  |
| 1 | `0x180131550` | method | `AddStringParam` |  | InvalidArgument (argument null) | catch-handler log |
| 2 | `0x180130dc0` | method | `AddIntParam` |  |  | catch-handler log |
| 3 | `0x180130c80` | method | `AddFloatParam` |  |  | catch-handler log |
| 4 | `0x180130b50` | method | `AddBoolParam` |  |  | catch-handler log |
| 5 | `0x180130ef0` | method | `AddObjectParam` |  |  | catch-handler log |
| 6 | `0x180130a00` | method | `AddArrayParam` |  |  | catch-handler log |
| 7 | `0x1801317a0` | method | `CloseParam` |  |  | catch-handler log |
| 8 | `0x1801316b0` | method | `ClearParams` |  |  | catch-handler log |
| 9 | `0x1801322e0` | method | `SetSamplingClass` |  | InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 10 | `0x1801321c0` | method | `SendTelemetryEvent` |  |  | catch-handler log |
| 11 | `0x180131dc0` | method | `SendAnonymousTelemetryEvent` |  |  | catch-handler log |
| 12 | `0x180131920` | method | `GetVisitID` |  |  | catch-handler log |
| 13 | `0x180131a30` | method | `GetVisitIDCopy`† | `buffer=%p, bufferLength=%u` |  | log string is a copy-paste of the previous slot; `buffer` argument marks the Copy variant |
| 14 | `0x180131ce0` | method | `ResetVisitID` |  |  | catch-handler log |

### `galaxy::api::IUnauthorizedAccessError` — `galaxy::peer::facade::$0A::galaxy::api::VIUnauthorizedAccessError::?$TypeAwareFacadeError`

vtable `0x1809bd7c0`, 4 slots (1 destructor, 1 method, 2 purecall); 0 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1800c0720` | destructor |  |  |  |  |
| 1 | `0x18066286c` | purecall |  |  |  |  |
| 2 | `0x18066286c` | purecall |  |  |  |  |
| 3 | `0x1800c1940` | method |  |  |  |  |

### `galaxy::api::IUser` — `galaxy::peer::facade::PeerUserFacade`

vtable `0x1809c2b30`, 31 slots (29 method, 1 stub, 1 thunk); 29 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1800f809c` | thunk |  |  |  |  |
| 1 | `0x18014ca20` | method | `SignedIn` |  |  | catch-handler log |
| 2 | `0x18014af20` | method | `GetGalaxyID` |  |  | catch-handler log |
| 3 | `0x1800f9470` | method | `SignInCredentials` |  | InvalidArgument (argument empty, argument null); InvalidState (already signing in) | catch-handler log |
| 4 | `0x1800fa050` | method | `SignInToken` |  | InvalidArgument (argument empty, argument null); InvalidState (already signing in) | catch-handler log |
| 5 | `0x1800f9d90` | method | `SignInSteam` |  | InvalidArgument (argument empty, argument null); InvalidState (already signing in) | catch-handler log |
| 6 | `0x1800f9660` | method | `SignInEpic` |  | InvalidArgument (argument empty, argument null); InvalidState (already signing in) | catch-handler log |
| 7 | `0x1800f9850` | method | `SignInGalaxy` |  | InvalidState (already signing in) | catch-handler log |
| 8 | `0x1800fa230` | method | `SignInUWP` |  | Runtime (not available on this platform) | catch-handler log |
| 9 | `0x1800f99f0` | method | `SignInPS4` |  | Runtime (not available on this platform) | catch-handler log |
| 10 | `0x1800fa330` | method | `SignInXB1` |  | Runtime (not available on this platform) | catch-handler log |
| 11 | `0x1800fa440` | method | `SignInXBLive` |  | InvalidArgument (argument empty, argument null); InvalidState (already signing in) | catch-handler log |
| 12 | `0x1800f9230` | stub | `SignInAnonymous`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 13 | `0x1800f92c0` | method | `SignInAnonymousTelemetry` |  | InvalidState (already signing in) | catch-handler log |
| 14 | `0x1800f9b00` | method | `SignInServerKey` |  | InvalidArgument (argument empty, argument null); InvalidState (already signing in) | catch-handler log |
| 15 | `0x1800fa770` | method | `SignOut` |  | UnauthorizedAccess (not signed in) | catch-handler log |
| 16 | `0x18014c840` | method | `RequestUserData` | `userID=%llu ` | UnauthorizedAccess (not logged on) | catch-handler log |
| 17 | `0x18014bd90` | method | `IsUserDataAvailable` | `userID=%llu ` |  | catch-handler log |
| 18 | `0x18014b1b0` | method | `GetUserData` | `userID=%llu, key=%s` | InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 19 | `0x18014b790` | method | `GetUserDataCopy` | `userID=%llu, key=%s, buffer=%p, bufferLength=%u` | InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 20 | `0x1800f8c60` | method | `SetUserData` | `key=%s, value=(%u bytes)` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 21 | `0x18014baa0` | method | `GetUserDataCount` | `userID=%llu` |  | catch-handler log |
| 22 | `0x18014b4c0` | method | `GetUserDataByIndex` | `index=%u, key=%p, keyLength=%u, value=%p, valueLength=%u` | InvalidArgument (argument null) | catch-handler log |
| 23 | `0x1800f8630` | method | `DeleteUserData` | `key=%s` | UnauthorizedAccess (not logged on); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 24 | `0x18014bc90` | method | `IsLoggedOn` |  |  | catch-handler log |
| 25 | `0x18014c550` | method | `RequestEncryptedAppTicket` | `data=%p, dataSize=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument too large) | catch-handler log |
| 26 | `0x18014adb0` | method | `GetEncryptedAppTicket` | `encryptedAppTicket=%p, maxEncryptedAppTicketSize=%u, currentEncryptedAppTicketSize=%u` | InvalidArgument (argument null) | catch-handler log |
| 27 | `0x18014b050` | method | `GetSessionID` |  | UnauthorizedAccess (not logged on) | catch-handler log |
| 28 | `0x18014a970` | method | `GetAccessToken` |  | UnauthorizedAccess (not logged on) | catch-handler log |
| 29 | `0x18014ab80` | method | `GetAccessTokenCopy` | `buffer=%p, bufferLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |
| 30 | `0x18014bf70` | method | `ReportInvalidAccessToken` | `accessToken=(%u bytes)` | InvalidArgument (argument empty, argument null) | catch-handler log |

### `galaxy::api::IUser` — `galaxy::peer::facade::GameServerUserFacade`

vtable `0x1809cfe80`, 31 slots (16 method, 14 stub, 1 thunk); 16 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x18014385c` | thunk |  |  |  |  |
| 1 | `0x18014ca20` | method | `SignedIn` |  |  | catch-handler log |
| 2 | `0x18014af20` | method | `GetGalaxyID` |  |  | catch-handler log |
| 3 | `0x180143e60` | stub | `SignInCredentials`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 4 | `0x1801441c0` | stub | `SignInToken`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 5 | `0x180144130` | stub | `SignInSteam`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 6 | `0x180143ef0` | stub | `SignInEpic`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 7 | `0x180143f80` | stub | `SignInGalaxy`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 8 | `0x180144250` | stub | `SignInUWP`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 9 | `0x180144010` | stub | `SignInPS4`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 10 | `0x1801442e0` | stub | `SignInXB1`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 11 | `0x180144370` | stub | `SignInXBLive`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 12 | `0x180143c20` | method | `SignInAnonymous` |  | InvalidState (already signing in) | catch-handler log |
| 13 | `0x180143dd0` | stub | `SignInAnonymousTelemetry`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 14 | `0x1801440a0` | stub | `SignInServerKey`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 15 | `0x180144400` | stub | `SignOut`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 16 | `0x18014c840` | method | `RequestUserData` | `userID=%llu ` | UnauthorizedAccess (not logged on) | catch-handler log |
| 17 | `0x18014bd90` | method | `IsUserDataAvailable` | `userID=%llu ` |  | catch-handler log |
| 18 | `0x1801439f0` | method | `GetUserData` | `userID=%llu, key=%s` |  | catch-handler log |
| 19 | `0x18014b790` | method | `GetUserDataCopy` | `userID=%llu, key=%s, buffer=%p, bufferLength=%u` | InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 20 | `0x180143b90` | stub | `SetUserData`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 21 | `0x18014baa0` | method | `GetUserDataCount` | `userID=%llu` |  | catch-handler log |
| 22 | `0x18014b4c0` | method | `GetUserDataByIndex` | `index=%u, key=%p, keyLength=%u, value=%p, valueLength=%u` | InvalidArgument (argument null) | catch-handler log |
| 23 | `0x180143960` | stub | `DeleteUserData`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 24 | `0x18014bc90` | method | `IsLoggedOn` |  |  | catch-handler log |
| 25 | `0x18014c550` | method | `RequestEncryptedAppTicket` | `data=%p, dataSize=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument too large) | catch-handler log |
| 26 | `0x18014adb0` | method | `GetEncryptedAppTicket` | `encryptedAppTicket=%p, maxEncryptedAppTicketSize=%u, currentEncryptedAppTicketSize=%u` | InvalidArgument (argument null) | catch-handler log |
| 27 | `0x18014b050` | method | `GetSessionID` |  | UnauthorizedAccess (not logged on) | catch-handler log |
| 28 | `0x18014a970` | method | `GetAccessToken` |  | UnauthorizedAccess (not logged on) | catch-handler log |
| 29 | `0x18014ab80` | method | `GetAccessTokenCopy` | `buffer=%p, bufferLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |
| 30 | `0x18014bf70` | method | `ReportInvalidAccessToken` | `accessToken=(%u bytes)` | InvalidArgument (argument empty, argument null) | catch-handler log |

### `galaxy::api::IUser` — `galaxy::peer::facade::UserFacadeBase`

vtable `0x1809d15b0`, 31 slots (15 method, 15 purecall, 1 thunk); 15 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x18014a7ec` | thunk |  |  |  |  |
| 1 | `0x18014ca20` | method | `SignedIn` |  |  | catch-handler log |
| 2 | `0x18014af20` | method | `GetGalaxyID` |  |  | catch-handler log |
| 3 | `0x18066286c` | purecall | `SignInCredentials`† |  |  | same slot of a sibling vtable |
| 4 | `0x18066286c` | purecall | `SignInToken`† |  |  | same slot of a sibling vtable |
| 5 | `0x18066286c` | purecall | `SignInSteam`† |  |  | same slot of a sibling vtable |
| 6 | `0x18066286c` | purecall | `SignInEpic`† |  |  | same slot of a sibling vtable |
| 7 | `0x18066286c` | purecall | `SignInGalaxy`† |  |  | same slot of a sibling vtable |
| 8 | `0x18066286c` | purecall | `SignInUWP`† |  |  | same slot of a sibling vtable |
| 9 | `0x18066286c` | purecall | `SignInPS4`† |  |  | same slot of a sibling vtable |
| 10 | `0x18066286c` | purecall | `SignInXB1`† |  |  | same slot of a sibling vtable |
| 11 | `0x18066286c` | purecall | `SignInXBLive`† |  |  | same slot of a sibling vtable |
| 12 | `0x18066286c` | purecall | `SignInAnonymous`† |  |  | same slot of a sibling vtable |
| 13 | `0x18066286c` | purecall | `SignInAnonymousTelemetry`† |  |  | same slot of a sibling vtable |
| 14 | `0x18066286c` | purecall | `SignInServerKey`† |  |  | same slot of a sibling vtable |
| 15 | `0x18066286c` | purecall | `SignOut`† |  |  | same slot of a sibling vtable |
| 16 | `0x18014c840` | method | `RequestUserData` | `userID=%llu ` | UnauthorizedAccess (not logged on) | catch-handler log |
| 17 | `0x18014bd90` | method | `IsUserDataAvailable` | `userID=%llu ` |  | catch-handler log |
| 18 | `0x18014b1b0` | method | `GetUserData` | `userID=%llu, key=%s` | InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 19 | `0x18014b790` | method | `GetUserDataCopy` | `userID=%llu, key=%s, buffer=%p, bufferLength=%u` | InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 20 | `0x18066286c` | purecall | `SetUserData`† |  |  | same slot of a sibling vtable |
| 21 | `0x18014baa0` | method | `GetUserDataCount` | `userID=%llu` |  | catch-handler log |
| 22 | `0x18014b4c0` | method | `GetUserDataByIndex` | `index=%u, key=%p, keyLength=%u, value=%p, valueLength=%u` | InvalidArgument (argument null) | catch-handler log |
| 23 | `0x18066286c` | purecall | `DeleteUserData`† |  |  | same slot of a sibling vtable |
| 24 | `0x18014bc90` | method | `IsLoggedOn` |  |  | catch-handler log |
| 25 | `0x18014c550` | method | `RequestEncryptedAppTicket` | `data=%p, dataSize=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument too large) | catch-handler log |
| 26 | `0x18014adb0` | method | `GetEncryptedAppTicket` | `encryptedAppTicket=%p, maxEncryptedAppTicketSize=%u, currentEncryptedAppTicketSize=%u` | InvalidArgument (argument null) | catch-handler log |
| 27 | `0x18014b050` | method | `GetSessionID` |  | UnauthorizedAccess (not logged on) | catch-handler log |
| 28 | `0x18014a970` | method | `GetAccessToken` |  | UnauthorizedAccess (not logged on) | catch-handler log |
| 29 | `0x18014ab80` | method | `GetAccessTokenCopy` | `buffer=%p, bufferLength=%u` | UnauthorizedAccess (not logged on); InvalidArgument (argument null) | catch-handler log |
| 30 | `0x18014bf70` | method | `ReportInvalidAccessToken` | `accessToken=(%u bytes)` | InvalidArgument (argument empty, argument null) | catch-handler log |

### `galaxy::api::IUtils` — `galaxy::peer::facade::PeerUtilsFacade`

vtable `0x1809cb7b0`, 10 slots (9 method, 1 thunk); 9 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x180125ea0` | thunk |  |  |  |  |
| 1 | `0x1801f68d0` | method | `GetImageSize` | `imageID=%u` |  | catch-handler log |
| 2 | `0x1801f6770` | method | `GetImageRGBA` | `imageID=%u, buffer=%p, bufferLength=%u` | InvalidArgument (argument null) | catch-handler log |
| 3 | `0x180126800` | method | `RegisterForNotification` | `type=%s` | UnauthorizedAccess (not logged on); InvalidArgument (argument null, argument too large) | catch-handler log |
| 4 | `0x180126400` | method | `GetNotification` | `notificationID=%llu, typeLength=%u, contentSize=%u ` | InvalidArgument (argument null) | catch-handler log |
| 5 | `0x180126980` | method | `ShowOverlayWithWebPage` | `url=%s` | InvalidState (not signed in with Galaxy); UnauthorizedAccess (not logged on, not signed in); InvalidArgument (argument empty, argument null, argument too large) | catch-handler log |
| 6 | `0x180126680` | method | `IsOverlayVisible` |  |  | catch-handler log |
| 7 | `0x180126580` | method | `GetOverlayState` |  |  | catch-handler log |
| 8 | `0x180126130` | method | `DisableOverlayPopups` |  | InvalidState (not signed in with Galaxy); UnauthorizedAccess (not signed in); InvalidArgument (argument empty, argument null) | catch-handler log |
| 9 | `0x1801f6670` | method | `GetGogServicesConnectionState` |  |  | catch-handler log |

### `galaxy::api::IUtils` — `galaxy::peer::facade::GameServerUtilsFacade`

vtable `0x1809d0630`, 10 slots (3 method, 6 stub, 1 thunk); 3 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x180144dc0` | thunk |  |  |  |  |
| 1 | `0x1801f68d0` | method | `GetImageSize` | `imageID=%u` |  | catch-handler log |
| 2 | `0x1801f6770` | method | `GetImageRGBA` | `imageID=%u, buffer=%p, bufferLength=%u` | InvalidArgument (argument null) | catch-handler log |
| 3 | `0x1801450f0` | stub | `RegisterForNotification`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 4 | `0x180144f10` | stub | `GetNotification`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 5 | `0x180145180` | stub | `ShowOverlayWithWebPage`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 6 | `0x180145050` | stub | `IsOverlayVisible`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 7 | `0x180144fb0` | stub | `GetOverlayState`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 8 | `0x180144e80` | stub | `DisableOverlayPopups`† |  | InvalidState (not implemented on current facade) | same slot of a sibling vtable |
| 9 | `0x1801f6670` | method | `GetGogServicesConnectionState` |  |  | catch-handler log |

### `galaxy::api::IUtils` — `galaxy::peer::facade::UtilsFacadeBase`

vtable `0x1809e3988`, 10 slots (3 method, 6 purecall, 1 thunk); 3 implemented slots named.

| Slot | Function | Class | Method | Arguments | Errors | Basis |
|---|---|---|---|---|---|---|
| 0 | `0x1801f65b0` | thunk |  |  |  |  |
| 1 | `0x1801f68d0` | method | `GetImageSize` | `imageID=%u` |  | catch-handler log |
| 2 | `0x1801f6770` | method | `GetImageRGBA` | `imageID=%u, buffer=%p, bufferLength=%u` | InvalidArgument (argument null) | catch-handler log |
| 3 | `0x18066286c` | purecall | `RegisterForNotification`† |  |  | same slot of a sibling vtable |
| 4 | `0x18066286c` | purecall | `GetNotification`† |  |  | same slot of a sibling vtable |
| 5 | `0x18066286c` | purecall | `ShowOverlayWithWebPage`† |  |  | same slot of a sibling vtable |
| 6 | `0x18066286c` | purecall | `IsOverlayVisible`† |  |  | same slot of a sibling vtable |
| 7 | `0x18066286c` | purecall | `GetOverlayState`† |  |  | same slot of a sibling vtable |
| 8 | `0x18066286c` | purecall | `DisableOverlayPopups`† |  |  | same slot of a sibling vtable |
| 9 | `0x1801f6670` | method | `GetGogServicesConnectionState` |  |  | catch-handler log |

