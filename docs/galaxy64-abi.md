# `Galaxy64.dll` ABI map

Derived from the binary rather than from headers: GOG does not publish
the Galaxy SDK without a registered developer account. Everything below
comes from parsing the PE, walking its RTTI, and reading the logging
strings each method passes carrying its own name.

Regenerate with `tools/gen_abi_doc.py <path-to-Galaxy64.dll> docs/galaxy64-abi.md`. The binary
itself is **not** in this repo (15 MB, proprietary to GOG) — copy it out
of an installed game.

## Why the names here are not guesses

Every Galaxy method body passes its own name as a string to an internal
logging call. `IStats::SetAchievement`, for instance, references the
literal `"SetAchievement"` and a format string reading
`Setting achievement: achievementID=%s, name=%s`. So each vtable slot is
resolved by disassembling for rip-relative string references and taking
the first identifier-shaped one — the binary naming its own methods,
not an inference from declaration order.

For `IStats` this was additionally cross-checked against GOG's published
method list. All 35 slots agree.

## The binary

| | |
|---|---|
| Format | PE32+ (x86-64), DLL |
| Image base | `0x180000000` |
| Sections | 11 |
| Exports | 28 |
| vtables carrying RTTI | 2550 |
| `galaxy::api` interfaces | 27 |

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
| 5 | `?GameServerListenerRegistrar@api@galaxy@@YAPEAVIListenerRegistrar@12@XZ` | `0x1800cb550` | `GalaxyGameServerFacade` |
| 6 | `?GameServerLogger@api@galaxy@@YAPEAVILogger@12@XZ` | `0x1800cb660` | singleton `0x180dc6320` + `0x58` |
| 7 | `?GameServerMatchmaking@api@galaxy@@YAPEAVIMatchmaking@12@XZ` | `0x1800cb680` | singleton `0x180dc6300` + `0x58` |
| 8 | `?GameServerNetworking@api@galaxy@@YAPEAVINetworking@12@XZ` | `0x1800cb6a0` | singleton `0x180dc6308` + `0x58` |
| 9 | `?GameServerTelemetry@api@galaxy@@YAPEAVITelemetry@12@XZ` | `0x1800cb6c0` | singleton `0x180dc6318` + `0x58` |
| 10 | `?GameServerUser@api@galaxy@@YAPEAVIUser@12@XZ` | `0x1800cb6e0` | singleton `0x180dc62f8` + `0x58` |
| 11 | `?GameServerUtils@api@galaxy@@YAPEAVIUtils@12@XZ` | `0x1800cb700` | singleton `0x180dc6310` + `0x58` |
| 12 | `?GetError@api@galaxy@@YAPEBVIError@12@XZ` | `0x180143520` | `GameServerUserFacade` |
| 13 | `?Init@api@galaxy@@YAXAEBUInitOptions@12@@Z` | `0x1800c1950` | `GalaxyFacade` |
| 14 | `?InitGameServer@api@galaxy@@YAXAEBUInitOptions@12@@Z` | `0x1800cb740` | `GalaxyGameServerFacade` |
| 15 | `?ListenerRegistrar@api@galaxy@@YAPEAVIListenerRegistrar@12@XZ` | `0x1800c23b0` | — |
| 16 | `?Logger@api@galaxy@@YAPEAVILogger@12@XZ` | `0x1800c26a0` | singleton `0x180dc61d8` + `0x58` |
| 17 | `?Matchmaking@api@galaxy@@YAPEAVIMatchmaking@12@XZ` | `0x1800c26c0` | singleton `0x180dc61a0` + `0x58` |
| 18 | `?Networking@api@galaxy@@YAPEAVINetworking@12@XZ` | `0x1800c26e0` | singleton `0x180dc61a8` + `0x58` |
| 19 | `?ProcessData@api@galaxy@@YAXXZ` | `0x1800c2700` | — |
| 20 | `?ProcessGameServerData@api@galaxy@@YAXXZ` | `0x1800cc140` | `GalaxyDefaultPool` |
| 21 | `?Shutdown@api@galaxy@@YAXXZ` | `0x1800c29e0` | — |
| 22 | `?ShutdownGameServer@api@galaxy@@YAXXZ` | `0x1800cc3c0` | `GalaxyDefaultPool` |
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

Slot 0 is always the virtual destructor — an *adjustor thunk*
(`sub rcx, <offset>; jmp ...`) whenever the interface does not sit at
offset 0 of the complete object, which is why it carries no name of its
own. A `?` elsewhere is a method whose body never logs its name;
in practice those are trivial getters.

Addresses are the real function bodies, with incremental-link `jmp`
thunks already followed through.

### `galaxy::api::IApps` — `galaxy::peer::facade::AppsFacade`

vtable `0x1809cd130`, 4 slots, 4 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x18012c334` | `External` |
| 1 | `0x18012c660` | `External` |
| 2 | `0x18012c3d0` | `External` |
| 3 | `0x18012c4f0` | `External` |

### `galaxy::api::IChat` — `galaxy::peer::facade::ChatFacade`

vtable `0x1809c6578`, 9 slots, 9 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x18010c350` | `GetChatRoomMemberCount` |
| 1 | `0x18010d6b0` | `RequestChatRoomWithUser` |
| 2 | `0x18010d4b0` | `RequestChatRoomMessages` |
| 3 | `0x18010d7e0` | `SendChatRoomMessage` |
| 4 | `0x18010cba0` | `GetChatRoomMessageByIndex` |
| 5 | `0x18010c740` | `GetChatRoomMemberCount` |
| 6 | `0x18010c930` | `GetChatRoomMemberUserIDByIndex` |
| 7 | `0x18010cea0` | `GetChatRoomUnreadMessageCount` |
| 8 | `0x18010d200` | `MarkChatRoomAsRead` |

### `galaxy::api::ICustomNetworking` — `galaxy::peer::facade::CustomNetworkingFacade`

vtable `0x1809ccc68`, 8 slots, 8 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x18012ad60` | `CloseConnection` |
| 1 | `0x18012b170` | `OpenConnection` |
| 2 | `0x18012af20` | `CloseConnection` |
| 3 | `0x18012bc60` | `SendData` |
| 4 | `0x18012b040` | `GetAvailableDataSize` |
| 5 | `0x18012b590` | `PeekData` |
| 6 | `0x18012bac0` | `ReadData` |
| 7 | `0x18012b720` | `PopData` |

### `galaxy::api::IError` — `galaxy::api::IUnauthorizedAccessError`

vtable `0x1809bd000`, 4 slots, 0 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1800c0ea0` | *destructor* |
| 1 | `0x18066286c` | ? |
| 2 | `0x18066286c` | ? |
| 3 | `0x18066286c` | ? |

### `galaxy::api::IError` — `galaxy::api::IInvalidArgumentError`

vtable `0x1809bd030`, 4 slots, 0 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1800c0ce0` | *destructor* |
| 1 | `0x18066286c` | ? |
| 2 | `0x18066286c` | ? |
| 3 | `0x18066286c` | ? |

### `galaxy::api::IError` — `galaxy::api::IInvalidStateError`

vtable `0x1809bd060`, 4 slots, 0 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1800c0d60` | *destructor* |
| 1 | `0x18066286c` | ? |
| 2 | `0x18066286c` | ? |
| 3 | `0x18066286c` | ? |

### `galaxy::api::IError` — `galaxy::api::IRuntimeError`

vtable `0x1809bd090`, 4 slots, 0 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1800c0e20` | *destructor* |
| 1 | `0x18066286c` | ? |
| 2 | `0x18066286c` | ? |
| 3 | `0x18066286c` | ? |

### `galaxy::api::IFriends` — `galaxy::peer::facade::FriendsFacade`

vtable `0x1809c4f60`, 39 slots, 33 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1801032cc` | `ClearRichPresence` |
| 1 | `0x180104130` | ? |
| 2 | `0x180107f10` | `SetRichPresence` |
| 3 | `0x180107840` | `RequestUserInformation` |
| 4 | `0x1801065b0` | ? |
| 5 | `0x1801052c0` | `GetPersonaName` |
| 6 | `0x1801053f0` | `GetPersonaNameCopy` |
| 7 | `0x180105540` | ? |
| 8 | `0x180104de0` | `GetPersonaName` |
| 9 | `0x180104f40` | `GetPersonaName` |
| 10 | `0x1801050a0` | `GetPersonaName` |
| 11 | `0x1801044f0` | `GetFriendByIndex` |
| 12 | `0x180104670` | `GetFriendByIndex` |
| 13 | `0x180104230` | ? |
| 14 | `0x180104380` | `GetFriendByIndex` |
| 15 | `0x180106280` | `IsUserInTheSameGame` |
| 16 | `0x180107250` | `RequestFriendList` |
| 17 | `0x180106100` | `IsFriend` |
| 18 | `0x1801049c0` | `GetFriendCount` |
| 19 | `0x1801047f0` | `GetFriendByIndex` |
| 20 | `0x180107c90` | `SendFriendInvitation` |
| 21 | `0x180107130` | `RequestFriendInvitationList` |
| 22 | `0x180107720` | `RequestSentFriendInvitationList` |
| 23 | `0x180104ca0` | `GetFriendInvitationCount` |
| 24 | `0x180104ad0` | `GetFriendInvitationByIndex` |
| 25 | `0x180107b60` | `RespondToFriendInvitation` |
| 26 | `0x180103ac0` | `DeleteFriend` |
| 27 | `0x180108100` | `SetRichPresence` |
| 28 | `0x180103bf0` | `DeleteRichPresence` |
| 29 | `0x1801037d0` | `ClearRichPresence` |
| 30 | `0x1801074a0` | `RequestRichPresence` |
| 31 | `0x180105640` | ? |
| 32 | `0x180105c00` | `IsFriend` |
| 33 | `0x180105f10` | `IsFriend` |
| 34 | `0x180105940` | ? |
| 35 | `0x1801086a0` | `ShowOverlayInviteDialog` |
| 36 | `0x180107dc0` | `SendInvitation` |
| 37 | `0x180104000` | `FindUser` |
| 38 | `0x1801063d0` | `IsUserInTheSameGame` |

### `galaxy::api::IInvalidArgumentError` — `galaxy::peer::facade::$00::galaxy::api::VIInvalidArgumentError::?$TypeAwareFacadeError`

vtable `0x1809bd8a8`, 4 slots, 1 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1800c0660` | *destructor* |
| 1 | `0x18066286c` | ? |
| 2 | `0x18066286c` | ? |
| 3 | `0x1800c1910` | `GalaxyFacade` |

### `galaxy::api::IInvalidStateError` — `galaxy::peer::facade::$01::galaxy::api::VIInvalidStateError::?$TypeAwareFacadeError`

vtable `0x1809bd988`, 4 slots, 1 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1800c06a0` | *destructor* |
| 1 | `0x18066286c` | ? |
| 2 | `0x18066286c` | ? |
| 3 | `0x1800c1920` | `GalaxyFacade` |

### `galaxy::api::IListenerRegistrar` — `galaxy::peer::facade::FacadeListenerRegistrar`

vtable `0x1809c1358`, 3 slots, 0 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1800dd460` | *destructor* |
| 1 | `0x1800e1590` | ? |
| 2 | `0x1800e2cf0` | ? |

### `galaxy::api::ILogger` — `galaxy::peer::facade::LoggerFacade`

vtable `0x1809cd398`, 7 slots, 0 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x18012c890` | *destructor* |
| 1 | `0x18012d010` | ? |
| 2 | `0x18012c990` | ? |
| 3 | `0x18012ce70` | ? |
| 4 | `0x18012d1a0` | ? |
| 5 | `0x18012cb30` | ? |
| 6 | `0x18012ccd0` | ? |

### `galaxy::api::IMatchmaking` — `galaxy::peer::facade::PeerMatchmakingFacade`

vtable `0x1809c76b8`, 34 slots, 34 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x180111794` | `AddRequestLobbyListNearValueFilter` |
| 1 | `0x1801ecec0` | `CreateLobby` |
| 2 | `0x180113c30` | `RequestLobbyList` |
| 3 | `0x180111f30` | `AddRequestLobbyListResultCountFilter` |
| 4 | `0x180112080` | `AddRequestLobbyListStringFilter` |
| 5 | `0x180111c70` | `AddRequestLobbyListNumericalFilter` |
| 6 | `0x180111a30` | `AddRequestLobbyListNearValueFilter` |
| 7 | `0x180112510` | `GetLobbyByIndex` |
| 8 | `0x1801130b0` | `JoinLobby` |
| 9 | `0x1801f0f50` | `LeaveLobby` |
| 10 | `0x1801f30d0` | `SetMaxNumLobbyMembers` |
| 11 | `0x1801efeb0` | `GetMaxNumLobbyMembers` |
| 12 | `0x1801f00f0` | `GetNumLobbyMembers` |
| 13 | `0x1801ee0b0` | `GetLobbyMemberByIndex` |
| 14 | `0x1801f29d0` | `SetLobbyType` |
| 15 | `0x1801efc50` | `GetLobbyType` |
| 16 | `0x1801f2330` | `SetLobbyJoinable` |
| 17 | `0x1801f0d10` | `IsLobbyJoinable` |
| 18 | `0x180113570` | `RequestLobbyData` |
| 19 | `0x1801ed380` | `GetLobbyData` |
| 20 | `0x1801eda40` | `GetLobbyDataCopy` |
| 21 | `0x1801f22e0` | `SetLobbyData` |
| 22 | `0x1801ede70` | `GetLobbyDataCount` |
| 23 | `0x1801ed700` | `GetLobbyDataByIndex` |
| 24 | `0x1801ed330` | `DeleteLobbyData` |
| 25 | `0x1801ee410` | `GetLobbyMemberData` |
| 26 | `0x1801eec50` | `GetLobbyMemberDataCopy` |
| 27 | `0x180113da0` | `SetLobbyMemberData` |
| 28 | `0x1801ef230` | `GetLobbyMemberDataCount` |
| 29 | `0x1801ee850` | `GetLobbyMemberDataByIndex` |
| 30 | `0x180112350` | `DeleteLobbyMemberData` |
| 31 | `0x1801ef7f0` | `GetLobbyOwner` |
| 32 | `0x1801f1fc0` | `SendLobbyMessage` |
| 33 | `0x1801ef510` | `GetLobbyMessage` |

### `galaxy::api::IMatchmaking` — `galaxy::peer::facade::GameServerMatchmakingFacade`

vtable `0x1809d02b8`, 34 slots, 30 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1801445b0` | *destructor* |
| 1 | `0x180144850` | `GameServerUtilsFacade` |
| 2 | `0x180144c60` | `GameServerUtilsFacade` |
| 3 | `0x180144730` | ? |
| 4 | `0x1801447c0` | `GameServerUtilsFacade` |
| 5 | `0x1801446a0` | ? |
| 6 | `0x180144610` | ? |
| 7 | `0x180144a80` | `GameServerUtilsFacade` |
| 8 | `0x180144b40` | `GameServerUtilsFacade` |
| 9 | `0x1801f0f50` | `LeaveLobby` |
| 10 | `0x1801f30d0` | `SetMaxNumLobbyMembers` |
| 11 | `0x1801efeb0` | `GetMaxNumLobbyMembers` |
| 12 | `0x1801f00f0` | `GetNumLobbyMembers` |
| 13 | `0x1801ee0b0` | `GetLobbyMemberByIndex` |
| 14 | `0x1801f29d0` | `SetLobbyType` |
| 15 | `0x1801efc50` | `GetLobbyType` |
| 16 | `0x1801f2330` | `SetLobbyJoinable` |
| 17 | `0x1801f0d10` | `IsLobbyJoinable` |
| 18 | `0x180144bd0` | `GameServerUtilsFacade` |
| 19 | `0x1801ed380` | `GetLobbyData` |
| 20 | `0x1801eda40` | `GetLobbyDataCopy` |
| 21 | `0x1801f22e0` | `SetLobbyData` |
| 22 | `0x1801ede70` | `GetLobbyDataCount` |
| 23 | `0x1801ed700` | `GetLobbyDataByIndex` |
| 24 | `0x1801ed330` | `DeleteLobbyData` |
| 25 | `0x1801ee410` | `GetLobbyMemberData` |
| 26 | `0x1801eec50` | `GetLobbyMemberDataCopy` |
| 27 | `0x180144cf0` | `GameServerUtilsFacade` |
| 28 | `0x1801ef230` | `GetLobbyMemberDataCount` |
| 29 | `0x1801ee850` | `GetLobbyMemberDataByIndex` |
| 30 | `0x1801449f0` | `GameServerUtilsFacade` |
| 31 | `0x1801ef7f0` | `GetLobbyOwner` |
| 32 | `0x1801f1fc0` | `SendLobbyMessage` |
| 33 | `0x1801ef510` | `GetLobbyMessage` |

### `galaxy::api::IMatchmaking` — `galaxy::peer::facade::MatchmakingFacadeBase`

vtable `0x1809e1dd8`, 34 slots, 24 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1801ecd20` | `CreateLobby` |
| 1 | `0x1801ecec0` | `CreateLobby` |
| 2 | `0x18066286c` | ? |
| 3 | `0x18066286c` | ? |
| 4 | `0x18066286c` | ? |
| 5 | `0x18066286c` | ? |
| 6 | `0x18066286c` | ? |
| 7 | `0x18066286c` | ? |
| 8 | `0x18066286c` | ? |
| 9 | `0x1801f0f50` | `LeaveLobby` |
| 10 | `0x1801f30d0` | `SetMaxNumLobbyMembers` |
| 11 | `0x1801efeb0` | `GetMaxNumLobbyMembers` |
| 12 | `0x1801f00f0` | `GetNumLobbyMembers` |
| 13 | `0x1801ee0b0` | `GetLobbyMemberByIndex` |
| 14 | `0x1801f29d0` | `SetLobbyType` |
| 15 | `0x1801efc50` | `GetLobbyType` |
| 16 | `0x1801f2330` | `SetLobbyJoinable` |
| 17 | `0x1801f0d10` | `IsLobbyJoinable` |
| 18 | `0x18066286c` | ? |
| 19 | `0x1801ed380` | `GetLobbyData` |
| 20 | `0x1801eda40` | `GetLobbyDataCopy` |
| 21 | `0x1801f22e0` | `SetLobbyData` |
| 22 | `0x1801ede70` | `GetLobbyDataCount` |
| 23 | `0x1801ed700` | `GetLobbyDataByIndex` |
| 24 | `0x1801ed330` | `DeleteLobbyData` |
| 25 | `0x1801ee410` | `GetLobbyMemberData` |
| 26 | `0x1801eec50` | `GetLobbyMemberDataCopy` |
| 27 | `0x18066286c` | ? |
| 28 | `0x1801ef230` | `GetLobbyMemberDataCount` |
| 29 | `0x1801ee850` | `GetLobbyMemberDataByIndex` |
| 30 | `0x18066286c` | ? |
| 31 | `0x1801ef7f0` | `GetLobbyOwner` |
| 32 | `0x1801f1fc0` | `SendLobbyMessage` |
| 33 | `0x1801ef510` | `GetLobbyMessage` |

### `galaxy::api::INetworking` — `galaxy::peer::facade::NetworkingFacade`

vtable `0x1809c8548`, 10 slots, 10 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1801169b8` | `GetConnectionType` |
| 1 | `0x180118330` | `SendP2PPacket` |
| 2 | `0x180117bb0` | `PeekP2PPacket` |
| 3 | `0x180117980` | `IsP2PPacketAvailable` |
| 4 | `0x180118040` | `ReadP2PPacket` |
| 5 | `0x180117e40` | `PopP2PPacket` |
| 6 | `0x180117230` | `GetPingWith` |
| 7 | `0x1801181f0` | `RequestNatTypeDetection` |
| 8 | `0x180117110` | `GetNatType` |
| 9 | `0x180116d80` | `GetConnectionType` |

### `galaxy::api::IRuntimeError` — `galaxy::peer::facade::$02::galaxy::api::VIRuntimeError::?$TypeAwareFacadeError`

vtable `0x1809bda68`, 4 slots, 1 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1800c06e0` | *destructor* |
| 1 | `0x18066286c` | ? |
| 2 | `0x18066286c` | ? |
| 3 | `0x1800c1930` | `GalaxyFacade` |

### `galaxy::api::IStats` — `galaxy::peer::facade::StatsFacade`

vtable `0x1809c9e08`, 35 slots, 34 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x18011e230` | *destructor* |
| 1 | `0x1801214f0` | `RequestUserStatsAndAchievements` |
| 2 | `0x180120460` | `GetStatInt` |
| 3 | `0x180120230` | `GetStatFloat` |
| 4 | `0x1801223e0` | `SetStatInt` |
| 5 | `0x180122200` | `SetStatFloat` |
| 6 | `0x180123180` | `UpdateAvgRateStat` |
| 7 | `0x18011eda0` | `GetAchievement` |
| 8 | `0x180121b10` | `SetAchievement` |
| 9 | `0x18011e800` | `ClearAchievement` |
| 10 | `0x180122650` | `StoreStatsAndAchievements` |
| 11 | `0x180121950` | `ResetStatsAndAchievements` |
| 12 | `0x18011f290` | `GetAchievementDisplayName` |
| 13 | `0x18011f400` | `GetAchievementDisplayNameCopy` |
| 14 | `0x18011efb0` | `GetAchievementDescription` |
| 15 | `0x18011f120` | `GetAchievementDescriptionCopy` |
| 16 | `0x180120880` | `IsAchievementVisible` |
| 17 | `0x1801209e0` | `IsAchievementVisibleWhileLocked` |
| 18 | `0x1801213d0` | `RequestLeaderboards` |
| 19 | `0x18011f570` | `GetLeaderboardDisplayName` |
| 20 | `0x18011f6d0` | `GetLeaderboardDisplayNameCopy` |
| 21 | `0x18011fc70` | `GetLeaderboardSortMethod` |
| 22 | `0x18011f830` | `GetLeaderboardDisplayType` |
| 23 | `0x180121180` | `RequestLeaderboardEntriesGlobal` |
| 24 | `0x180120b50` | `RequestLeaderboardEntriesAroundUser` |
| 25 | `0x180120e70` | `RequestLeaderboardEntriesForUsers` |
| 26 | `0x18011fe10` | `GetRequestedLeaderboardEntry` |
| 27 | `0x18011ff60` | `GetRequestedLeaderboardEntryWithDetails` |
| 28 | `0x180121d30` | `SetLeaderboardScore` |
| 29 | `0x180121f60` | `SetLeaderboardScoreWithDetails` |
| 30 | `0x18011f9d0` | `GetLeaderboardEntryCount` |
| 31 | `0x18011ea00` | `FindLeaderboard` |
| 32 | `0x18011eb40` | `FindOrCreateLeaderboard` |
| 33 | `0x180121720` | `RequestUserTimePlayed` |
| 34 | `0x180120680` | `GetUserTimePlayed` |

### `galaxy::api::IStorage` — `galaxy::peer::facade::StorageFacade`

vtable `0x1809cc238`, 20 slots, 18 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x180127e20` | `DownloadSharedFile` |
| 1 | `0x1801287b0` | `GetDownloadedSharedFileByIndex` |
| 2 | `0x1801282c0` | `FileShare` |
| 3 | `0x180128040` | `FileShare` |
| 4 | `0x180128190` | `FileShare` |
| 5 | `0x180128fa0` | `GetSharedFileName` |
| 6 | `0x180129180` | `GetSharedFileName` |
| 7 | `0x180128b40` | ? |
| 8 | `0x180128c40` | ? |
| 9 | `0x180128d60` | `GetSharedFileName` |
| 10 | `0x1801283e0` | `FileShare` |
| 11 | `0x180127f40` | `DownloadSharedFile` |
| 12 | `0x1801292b0` | `GetSharedFileName` |
| 13 | `0x180129400` | `GetSharedFileNameCopy` |
| 14 | `0x180129690` | `GetSharedFileSize` |
| 15 | `0x180129550` | `GetSharedFileOwner` |
| 16 | `0x180129ae0` | `SharedFileRead` |
| 17 | `0x1801299d0` | `SharedFileClose` |
| 18 | `0x180128a30` | `GetDownloadedSharedFileCount` |
| 19 | `0x180128910` | `GetDownloadedSharedFileByIndex` |

### `galaxy::api::ITelemetry` — `galaxy::peer::facade::TelemetryFacade`

vtable `0x1809ce0b8`, 15 slots, 2 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x180130918` | *destructor* |
| 1 | `0x180131550` | ? |
| 2 | `0x180130dc0` | ? |
| 3 | `0x180130c80` | ? |
| 4 | `0x180130b50` | ? |
| 5 | `0x180130ef0` | ? |
| 6 | `0x180130a00` | ? |
| 7 | `0x1801317a0` | ? |
| 8 | `0x1801316b0` | ? |
| 9 | `0x1801322e0` | `CountingAllocator` |
| 10 | `0x1801321c0` | `CountingAllocator` |
| 11 | `0x180131dc0` | ? |
| 12 | `0x180131920` | ? |
| 13 | `0x180131a30` | ? |
| 14 | `0x180131ce0` | ? |

### `galaxy::api::IUnauthorizedAccessError` — `galaxy::peer::facade::$0A::galaxy::api::VIUnauthorizedAccessError::?$TypeAwareFacadeError`

vtable `0x1809bd7c0`, 4 slots, 1 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1800c0720` | *destructor* |
| 1 | `0x18066286c` | ? |
| 2 | `0x18066286c` | ? |
| 3 | `0x1800c1940` | `GalaxyFacade` |

### `galaxy::api::IUser` — `galaxy::peer::facade::PeerUserFacade`

vtable `0x1809c2b30`, 31 slots, 22 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1800f809c` | *destructor* |
| 1 | `0x18014ca20` | ? |
| 2 | `0x18014af20` | `GetSessionID` |
| 3 | `0x1800f9470` | `SignInCredentials` |
| 4 | `0x1800fa050` | `SignInToken` |
| 5 | `0x1800f9d90` | `SignInSteam` |
| 6 | `0x1800f9660` | `SignInEpic` |
| 7 | `0x1800f9850` | `SignInGalaxy` |
| 8 | `0x1800fa230` | `SignInUWP` |
| 9 | `0x1800f99f0` | `SignInPS4` |
| 10 | `0x1800fa330` | `SignInXB1` |
| 11 | `0x1800fa440` | `SignInXBLive` |
| 12 | `0x1800f9230` | `SignInAnonymousTelemetry` |
| 13 | `0x1800f92c0` | `SignInAnonymousTelemetry` |
| 14 | `0x1800f9b00` | `SignInServerKey` |
| 15 | `0x1800fa770` | `SignOut` |
| 16 | `0x18014c840` | `RequestUserData` |
| 17 | `0x18014bd90` | ? |
| 18 | `0x18014b1b0` | ? |
| 19 | `0x18014b790` | ? |
| 20 | `0x1800f8c60` | `SetUserData` |
| 21 | `0x18014baa0` | ? |
| 22 | `0x18014b4c0` | ? |
| 23 | `0x1800f8630` | `DeleteUserData` |
| 24 | `0x18014bc90` | ? |
| 25 | `0x18014c550` | `RequestEncryptedAppTicket` |
| 26 | `0x18014adb0` | `GetSessionID` |
| 27 | `0x18014b050` | `GetSessionID` |
| 28 | `0x18014a970` | `GetAccessToken` |
| 29 | `0x18014ab80` | `GetAccessTokenCopy` |
| 30 | `0x18014bf70` | ? |

### `galaxy::api::IUser` — `galaxy::peer::facade::GameServerUserFacade`

vtable `0x1809cfe80`, 31 slots, 21 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x18014385c` | `SignInAnonymous` |
| 1 | `0x18014ca20` | ? |
| 2 | `0x18014af20` | `GetSessionID` |
| 3 | `0x180143e60` | ? |
| 4 | `0x1801441c0` | `GameServerMatchmakingFacade` |
| 5 | `0x180144130` | `GameServerMatchmakingFacade` |
| 6 | `0x180143ef0` | ? |
| 7 | `0x180143f80` | `GameServerMatchmakingFacade` |
| 8 | `0x180144250` | `GameServerMatchmakingFacade` |
| 9 | `0x180144010` | `GameServerMatchmakingFacade` |
| 10 | `0x1801442e0` | `GameServerMatchmakingFacade` |
| 11 | `0x180144370` | `GameServerMatchmakingFacade` |
| 12 | `0x180143c20` | `SignInAnonymous` |
| 13 | `0x180143dd0` | ? |
| 14 | `0x1801440a0` | `GameServerMatchmakingFacade` |
| 15 | `0x180144400` | `GameServerMatchmakingFacade` |
| 16 | `0x18014c840` | `RequestUserData` |
| 17 | `0x18014bd90` | ? |
| 18 | `0x1801439f0` | `SignInAnonymous` |
| 19 | `0x18014b790` | ? |
| 20 | `0x180143b90` | `SignInAnonymous` |
| 21 | `0x18014baa0` | ? |
| 22 | `0x18014b4c0` | ? |
| 23 | `0x180143960` | `SignInAnonymous` |
| 24 | `0x18014bc90` | ? |
| 25 | `0x18014c550` | `RequestEncryptedAppTicket` |
| 26 | `0x18014adb0` | `GetSessionID` |
| 27 | `0x18014b050` | `GetSessionID` |
| 28 | `0x18014a970` | `GetAccessToken` |
| 29 | `0x18014ab80` | `GetAccessTokenCopy` |
| 30 | `0x18014bf70` | ? |

### `galaxy::api::IUser` — `galaxy::peer::facade::UserFacadeBase`

vtable `0x1809d15b0`, 31 slots, 8 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x18014a7ec` | `GetAccessToken` |
| 1 | `0x18014ca20` | ? |
| 2 | `0x18014af20` | `GetSessionID` |
| 3 | `0x18066286c` | ? |
| 4 | `0x18066286c` | ? |
| 5 | `0x18066286c` | ? |
| 6 | `0x18066286c` | ? |
| 7 | `0x18066286c` | ? |
| 8 | `0x18066286c` | ? |
| 9 | `0x18066286c` | ? |
| 10 | `0x18066286c` | ? |
| 11 | `0x18066286c` | ? |
| 12 | `0x18066286c` | ? |
| 13 | `0x18066286c` | ? |
| 14 | `0x18066286c` | ? |
| 15 | `0x18066286c` | ? |
| 16 | `0x18014c840` | `RequestUserData` |
| 17 | `0x18014bd90` | ? |
| 18 | `0x18014b1b0` | ? |
| 19 | `0x18014b790` | ? |
| 20 | `0x18066286c` | ? |
| 21 | `0x18014baa0` | ? |
| 22 | `0x18014b4c0` | ? |
| 23 | `0x18066286c` | ? |
| 24 | `0x18014bc90` | ? |
| 25 | `0x18014c550` | `RequestEncryptedAppTicket` |
| 26 | `0x18014adb0` | `GetSessionID` |
| 27 | `0x18014b050` | `GetSessionID` |
| 28 | `0x18014a970` | `GetAccessToken` |
| 29 | `0x18014ab80` | `GetAccessTokenCopy` |
| 30 | `0x18014bf70` | ? |

### `galaxy::api::IUtils` — `galaxy::peer::facade::PeerUtilsFacade`

vtable `0x1809cb7b0`, 10 slots, 7 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x180125ea0` | `DisableOverlayPopups` |
| 1 | `0x1801f68d0` | ? |
| 2 | `0x1801f6770` | ? |
| 3 | `0x180126800` | `RegisterForNotification` |
| 4 | `0x180126400` | `RegisterForNotification` |
| 5 | `0x180126980` | `ShowOverlayWithWebPage` |
| 6 | `0x180126680` | `RegisterForNotification` |
| 7 | `0x180126580` | `RegisterForNotification` |
| 8 | `0x180126130` | `DisableOverlayPopups` |
| 9 | `0x1801f6670` | ? |

### `galaxy::api::IUtils` — `galaxy::peer::facade::GameServerUtilsFacade`

vtable `0x1809d0630`, 10 slots, 7 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x180144dc0` | `FacadeScheduler` |
| 1 | `0x1801f68d0` | ? |
| 2 | `0x1801f6770` | ? |
| 3 | `0x1801450f0` | `FacadeScheduler` |
| 4 | `0x180144f10` | `FacadeScheduler` |
| 5 | `0x180145180` | `FacadeScheduler` |
| 6 | `0x180145050` | `FacadeScheduler` |
| 7 | `0x180144fb0` | `FacadeScheduler` |
| 8 | `0x180144e80` | `FacadeScheduler` |
| 9 | `0x1801f6670` | ? |

### `galaxy::api::IUtils` — `galaxy::peer::facade::UtilsFacadeBase`

vtable `0x1809e3988`, 10 slots, 0 identified.

| Slot | Function | Method |
|---|---|---|
| 0 | `0x1801f65b0` | *destructor* |
| 1 | `0x1801f68d0` | ? |
| 2 | `0x1801f6770` | ? |
| 3 | `0x18066286c` | ? |
| 4 | `0x18066286c` | ? |
| 5 | `0x18066286c` | ? |
| 6 | `0x18066286c` | ? |
| 7 | `0x18066286c` | ? |
| 8 | `0x18066286c` | ? |
| 9 | `0x1801f6670` | ? |

