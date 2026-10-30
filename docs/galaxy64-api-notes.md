# `Galaxy64.dll` API notes

Hand-written companion to [`galaxy64-abi.md`](galaxy64-abi.md): what the six main
facades (IApps, IStats, IUser, IUtils, IFriends, IStorage) take, return and check,
read from the code of one build. This file is **not regenerated**; the ABI map is,
and it is the reference for slot numbers, classes and names of every interface.
If the DLL build changes, re-verify the addresses here before relying on them.

Build: `dll/Galaxy64.dll`, SHA-256 `cdc1dd81e6287b169b0921c5011add80bcd5512d0b9e5f977e178ef16d0d924a`, `.text` vsize `0x9b4f87`.
Read with REA 6.8 / Ghidra 12.1.4. `FUN_<address>` is Ghidra's auto-name for the function at that address.

Tags: **[P]** proven, read in code (pseudocode, bytes or strings of this build); **[I]** inferred
(SDK convention, parameter count, naming). Method names and slot numbers agree with the ABI map.
Rows tagged `[I]` in a name are the `†` rows there.

## Common contract (all six facades)

- Every implemented method opens with: static-init guard `FUN_1800ccdd0`, delete and clear the thread-local `IError*` at TLS+0x110, then a scope object `FUN_1800cc8c0`/`FUN_1800cc910` [P]. TLS+0x120 is the in-place buffer the error object is constructed into (name 0x400 bytes at +8, message at +0x408) [P: FUN_1800f7380].
- Every implemented method of these facades has the same FH3 try map: 10 catch handlers [P: 208 of 208 EH-bearing facade bodies]. Inner try: `galaxy::api::IError` -> RuntimeError "Unexpected %s facade exception in safe code block"; `...` -> "Unexpected unknown throwable in safe code block" and an error-level log "<Method>: <args>" [P: Catch@18093342c, Catch@180933489]. Outer try: `gog::IUnauthorizedAccessError`/`IInvalidArgumentError`/`IInvalidStateError`/`IError` -> store the matching galaxy error in TLS (FUN_1800bd090, bce10, bceb0, bcf50), then resume at the method's continuation; `...` -> "Inner error handling failure" [P]. Nothing propagates to the caller; the caller reads `galaxy::api::GetError()`.
- Error type codes (`IError::GetType`, slot 3 of the TypeAwareFacadeError vtables): UnauthorizedAccess 0 (`FUN_1800c1940`), InvalidArgument 1 (`1800c1910`), InvalidState 2 (`1800c1920`), Runtime 3 (`1800c1930`) [P].
- "Failed return" is what the continuation after the catch returns, decoded from bytes after the scope destructor call: `33 c0` 0, `32 c0` false, `0f 57 c0` 0.0f, `48 8d 05` -> `0x1809bc000` = "" [P bytes; Ghidra does not disassemble these continuations, so decoded by hand].
- Precondition helpers: `FUN_1801499e0` UA "%s: not logged on" (FUN_1800e1000 false); `FUN_180149a70` UA "%s: not signed in" (FUN_1800e1120 false); `FUN_180149c50` UA "%s: not signed in" (user object +0x10 == 0); `FUN_180149b00` = 149a70 then IS "%s: not signed in with Galaxy" (state != 3); `FUN_180149ba0` = 149a70 then IS "%s: not signed in with GOG" (state not in {1,3,8}, mask 0x10a); `FUN_1800cce50`/`1800ccfc0` IA "'%s' is null"; `FUN_1800cd100` IA "'%s' is empty"; `FUN_1800ccf80` null+empty; `FUN_1800cceb0`/`1800cd080` IA size: fails unless size+1 <= max ("has size of %llu bytes, which is more than the max size of %llu bytes"); `FUN_1800cd020` null + strlen size; `FUN_1800ccf40` null + strlen size + empty [P]. UA = gog::UnauthorizedAccessError, IA = gog::InvalidArgumentError, IS = gog::InvalidStateError, RT = gog::RuntimeError (ThrowInfo 0x180d27478, 0x180d26fb0, 0x180d26ef0, 0x180d26e08) [P]. The string passed to the UA/IS helpers is the method name [P].
- `FUN_1800cca70` copy: bufferLength 0 -> nothing written; else memset(buffer,0,bufferLength), copy min(len,bufferLength-1), terminate [P]. Binary copy `FUN_1800cc9c0`/`1800cc9f0`: memset whole buffer, copy min(len,bufferLength), no terminator, returns count [P].
- Argument names and order come from the catch-block log format "<Method>: a=%x, ..." [P]; C types are from the printf conversion plus Ghidra parameter count (`%llu` uint64: GalaxyID/ProductID/SharedFileID, `%u` uint32, `%d` int32, `%s` const char*, `%p` pointer) [I for exact SDK types]. Trailing listener pointers are present when Ghidra shows one parameter more than the log lists [I]. GalaxyID parameters appear as pointers in several Ghidra signatures (MSVC passes the class by reference to a temporary) [I].

## IApps — `AppsFacade` vtable `0x1809cd130`

| Slot | Method (args) | Returns / on failure | Preconditions -> error | Notes |
|---|---|---|---|---|
| 0 | destructor (adjustor thunk `0x18012c334`: sub rcx,0x58; jmp 0x18001e5a1) | — | — | [P] |
| 1 | `IsDlcInstalled`(uint64 productID) `0x18012c660` | bool; fail: local init 0 -> false [P] | none at facade level [P] | Delegates to FUN_180203cd0(this+8, productID) [P]. |
| 2 | `GetCurrentGameLanguage`(uint64 productID) `0x18012c3d0` | const char*; fail: local init "english" [P] | none at facade level [P] | On error returns the pre-initialised "english" [P: local init + continuation 0x18012c478 reads it]. |
| 3 | `GetCurrentGameLanguageCopy`(char* buffer, uint32 bufferLength, uint64 productID [P: Ghidra param order; log lists only productID]) `0x18012c4f0` | void | IA `buffer` null | Copy runs after the guarded block, also on error (then copies "english") [P: continuation 0x18012c5a8 enters the copy tail]. Copies min(strlen, bufferLength-1) with memcpy and writes the terminator; bufferLength==0 makes the clamp 0xFFFFFFFF, i.e. an unbounded copy [P: FUN_18012c4f0 tail]. Differs from the shared copy helper used elsewhere.  |

## IStats — `StatsFacade` vtable `0x1809c9e08`

| Slot | Method (args) | Returns / on failure | Preconditions -> error | Notes |
|---|---|---|---|---|
| 0 | destructor (adjustor thunk `0x18011e230`: sub rcx,0x58; jmp 0x18000b49c) | — | — | [P] |
| 1 | `RequestUserStatsAndAchievements`(uint64 userID, listener? [I]) `0x1801214f0` | void | UA "not signed in" | UA "requesting other user's data while n..." when userID is not the local user and not logged on [P string; condition I]. |
| 2 | `GetStatInt`(const char* name, uint64 userID) `0x180120460` | int32; fail: 0 [P] | UA "not signed in"; IA `name` null/empty | Throws IS "Reading stat \"%s\" with wrong data type" when the stat is not int (no method prefix) [P: body string]. |
| 3 | `GetStatFloat`(const char* name, uint64 userID) `0x180120230` | float; fail: 0.0f [P] | UA "not signed in"; IA `name` null/empty | IS "GetStatFloat: reading stat \"%s\" with wrong data type" [P]. |
| 4 | `SetStatInt`(const char* name, int32 value) `0x1801223e0` | void | UA "not signed in"; IA `name` null/empty | IS on wrong data type; logs "trying to set the same data that is already set" and does nothing when unchanged [P strings; control flow I]. |
| 5 | `SetStatFloat`(const char* name, float value) `0x180122200` | void | UA "not signed in"; IA `name` null/empty |  |
| 6 | `UpdateAvgRateStat`(const char* name, float countThisSession, double sessionLength) `0x180123180` | void | UA "not signed in"; IA `name` null/empty |  |
| 7 | `GetAchievement`(const char* name, uint64 userID, +2 more (listener/out params) [I]) `0x18011eda0` | void (out: bool* unlocked, uint32* unlockTime) | UA "not signed in"; IA `name` null/empty |  |
| 8 | `SetAchievement`(const char* name) `0x180121b10` | void | UA "not signed in"; IA `name` null/empty | Logs "Setting achievement: achievementID=%s, name=%s" [P]. |
| 9 | `ClearAchievement`(const char* name) `0x18011e800` | void | UA "not signed in"; IA `name` null/empty |  |
| 10 | `StoreStatsAndAchievements`(listener? [I]) `0x180122650` | void | UA "not signed in" |  |
| 11 | `ResetStatsAndAchievements`(listener? [I]) `0x180121950` | void | UA "not signed in" |  |
| 12 | `GetAchievementDisplayName`(const char* name) `0x18011f290` | const char*; fail: "" (0x1809bc000) [P] | UA "not signed in"; IA `name` null/empty |  |
| 13 | `GetAchievementDisplayNameCopy`(const char* name, ptr buffer, uint32 bufferLength) `0x18011f400` | void | UA "not signed in"; IA `name` null/empty; IA `buffer` null | copy helper FUN_1800cca70 [P]. |
| 14 | `GetAchievementDescription`(const char* name) `0x18011efb0` | const char*; fail: "" (0x1809bc000) [P] | UA "not signed in"; IA `name` null/empty |  |
| 15 | `GetAchievementDescriptionCopy`(const char* name, ptr buffer, uint32 bufferLength) `0x18011f120` | void | UA "not signed in"; IA `name` null/empty; IA `buffer` null | copy helper FUN_1800cca70 [P]. |
| 16 | `IsAchievementVisible`(const char* name) `0x180120880` | bool; fail: false [P] | UA "not signed in"; IA `name` null/empty |  |
| 17 | `IsAchievementVisibleWhileLocked`(const char* name) `0x1801209e0` | bool; fail: false [P] | UA "not signed in"; IA `name` null/empty |  |
| 18 | `RequestLeaderboards`(listener? [I]) `0x1801213d0` | void | UA "not logged on"; UA "not signed in" |  |
| 19 | `GetLeaderboardDisplayName`(const char* name) `0x18011f570` | const char*; fail: "" (0x1809bc000) [P] | UA "not signed in"; IA `name` null/empty |  |
| 20 | `GetLeaderboardDisplayNameCopy`(const char* name, ptr buffer, uint32 bufferLength) `0x18011f6d0` | void | UA "not signed in"; IA `name` null/empty; IA `buffer` null | copy helper FUN_1800cca70 [P]. |
| 21 | `GetLeaderboardSortMethod`(const char* name) `0x18011fc70` | enum SortMethod; fail: 0 [P] | UA "not signed in"; IA `name` null/empty |  |
| 22 | `GetLeaderboardDisplayType`(const char* name) `0x18011f830` | enum DisplayType; fail: 0 [P] | UA "not signed in"; IA `name` null/empty |  |
| 23 | `RequestLeaderboardEntriesGlobal`(const char* name, uint32 rangeStart, uint32 rangeEnd, listener? [I]) `0x180121180` | void | UA "not logged on"; UA "not signed in"; IA `name` null/empty |  |
| 24 | `RequestLeaderboardEntriesAroundUser`(const char* name, uint32 countBefore, uint32 countAfter, uint64 userID, listener? [I]) `0x180120b50` | void | UA "not logged on"; UA "not signed in"; IA `name` null/empty |  |
| 25 | `RequestLeaderboardEntriesForUsers`(const char* name, ptr userList, uint32 userCount, listener? [I]) `0x180120e70` | void | UA "not logged on"; UA "not signed in"; IA `name` null/empty |  |
| 26 | `GetRequestedLeaderboardEntry`(uint32 index, +3 more (listener/out params) [I]) `0x18011fe10` | void (out: uint32* rank, int32* score, GalaxyID* userID) | UA "not signed in" |  |
| 27 | `GetRequestedLeaderboardEntryWithDetails`(uint32 index, ptr details, uint32 detailsSize, +4 more (listener/out params) [I]) `0x18011ff60` | void (out: rank, score, details buffer, uint32* outDetailsSize, GalaxyID*) | UA "not signed in" | IS "Reading leaderboard entries outside of ILeaderboardEntriesRetrieveListener" and IA-like "Given index %u exceeds leaderboard entry count %llu" [P strings; classes partly I]. Details copied with the binary helper FUN_1800cc9c0 (no terminator) [P]. |
| 28 | `SetLeaderboardScore`(const char* name, int32 score, int32 forceUpdate, listener? [I]) `0x180121d30` | void | UA "not signed in"; IA `name` null/len/empty (len+1 <= 0x100) |  |
| 29 | `SetLeaderboardScoreWithDetails`(const char* name, int32 score, ptr details, uint32 destSize, int32 forceUpdate, listener? [I]) `0x180121f60` | void | UA "not signed in"; IA `name` null/len/empty (len+1 <= 0x100); IA `details` null; IA `detailsSize` size (len+1 <= 0xc00) | `details` null check and `detailsSize` must satisfy size+1 <= 0xc00 [P: FUN_1800cceb0 args]. |
| 30 | `GetLeaderboardEntryCount`(const char* name) `0x18011f9d0` | uint32; fail: 0 [P] | UA "not signed in"; IA `name` null/empty | IS "leaderboard not found, name=%s" / "leaderboard entry count not found" [P strings; class I]. |
| 31 | `FindLeaderboard`(const char* name, listener? [I]) `0x18011ea00` | void | UA "not logged on"; UA "not signed in"; IA `name` null/empty |  |
| 32 | `FindOrCreateLeaderboard`(const char* name, const char* displayName, uint32 sortMethod, uint32 displayType, listener? [I]) `0x18011eb40` | void | UA "not logged on"; UA "not signed in"; IA `name` null/empty; IA `displayName` null; IA "invalid sortMethod"; IA "invalid displayType" | IA "invalid sortMethod"/"invalid displayType" [P]. |
| 33 | `RequestUserTimePlayed`(uint64 userID, listener? [I]) `0x180121720` | void | UA "not signed in" | Same other-user UA check as RequestUserStatsAndAchievements [P string]. |
| 34 | `GetUserTimePlayed`(uint64 userID) `0x180120680` | uint32 (seconds; internal value /1000000); fail: 0 [P] | UA "not signed in" |  |

## IUser — `PeerUserFacade` vtable `0x1809c2b30`

| Slot | Method (args) | Returns / on failure | Preconditions -> error | Notes |
|---|---|---|---|---|
| 0 | destructor (adjustor thunk `0x1800f809c`: sub rcx,0x58; jmp 0x180004a16) | — | — | [P] |
| 1 | `SignedIn`() `0x18014ca20` | bool; fail: false [P] | none at facade level [P] |  |
| 2 | `GetGalaxyID`() `0x18014af20` | GalaxyID (sret); fail: GalaxyID 0 (xor ebx,ebx) [P] | none at facade level [P] | GalaxyID returned through the hidden return pointer; on error `xor ebx,ebx` at continuation 0x18014afd9 [P bytes] -> GalaxyID 0 [I]. |
| 3 | `SignInCredentials`(const char* login, const char* password, listener? [I]) `0x1800f9470` | void | IA `login` null/empty; IA `password` null/empty; IS "%s: already signing in"; IS "%s: already signed in" | IS "%s: already signing in" / "%s: already signed in" [P]; trailing IAuthListener* [I]. |
| 4 | `SignInToken`(const char* refreshToken, listener? [I]) `0x1800fa050` | void | IA `refreshToken` null/empty; IS "%s: already signing in"; IS "%s: already signed in" |  |
| 5 | `SignInSteam`(const void* steamAppTicket, uint32 steamAppTicketSize, const char* personaName, listener? [I]) `0x1800f9d90` | void | IA `steamAppTicket` null; IA `personaName` null/empty; IS "%s: already signing in"; IS "%s: already signed in" |  |
| 6 | `SignInEpic`(const char* epicAccessToken, const char* epicUsername, listener? [I]) `0x1800f9660` | void | IA `epicAccessToken` null/empty; IA `epicUsername` null/empty; IS "%s: already signing in"; IS "%s: already signed in" |  |
| 7 | `SignInGalaxy`(bool requireOnline [I], listener? [I]) `0x1800f9850` | void | IS "%s: already signing in"; IS "%s: already signed in" | Args (bool requireOnline, IAuthListener*) [I from 2 Ghidra params]; IS already signing/signed in [P]. |
| 8 | `SignInUWP`() `0x1800fa230` | void | RT "%s: method not available on this platform" | Throws RuntimeError "%s: method not available on this platform" [P]. Same for SignInPS4 (9) and SignInXB1 (10). |
| 9 | `SignInPS4`(listener? [I]) `0x1800f99f0` | void | RT "%s: method not available on this platform" | RT "method not available on this platform" [P]. |
| 10 | `SignInXB1`(listener? [I]) `0x1800fa330` | void | RT "%s: method not available on this platform" | RT "method not available on this platform" [P]. |
| 11 | `SignInXBLive`(const char* token, const char* signature [I], const char* marketplaceID, const char* locale, listener? [I]) `0x1800fa440` | void | IA `token` null/empty; IA `marketplaceID` null/empty; IA `locale` null/empty; IS "%s: already signing in"; IS "%s: already signed in" |  |
| 12 | `SignInAnonymous` [I: name taken from the same slot of the game-server facade](+3 more (listener/out params) [I]) `0x1800f9230` | void | none at facade level [P] | NOT IMPLEMENTED on the peer facade: sets GalaxyInvalidStateError "Specified method is not implemented on current facade." [P: FUN_1800f9230 contains the string; FUN_1800f7380 writes vtable GalaxyInvalidStateError]. |
| 13 | `SignInAnonymousTelemetry`(listener? [I]) `0x1800f92c0` | void | IS "%s: already signing in"; IS "%s: already signed in" |  |
| 14 | `SignInServerKey`(const char* serverKey, listener? [I]) `0x1800f9b00` | void | IA `serverKey` null/empty; IS "%s: already signing in"; IS "%s: already signed in" |  |
| 15 | `SignOut`() `0x1800fa770` | void | UA "not signed in" |  |
| 16 | `RequestUserData`(uint64 userID, listener? [I]) `0x18014c840` | void | UA "not logged on" |  |
| 17 | `IsUserDataAvailable`(uint64 userID) `0x18014bd90` | bool; fail: false [P] | none at facade level [P] |  |
| 18 | `GetUserData`(uint64 userID, const char* key) `0x18014b1b0` | const char*; fail: "" (0x1809bc000) [P] | IA `key` null/len/empty |  |
| 19 | `GetUserDataCopy`(uint64 userID, const char* key, ptr buffer, uint32 bufferLength) `0x18014b790` | void | IA `key` null/len/empty (len+1 <= 0x400); IA `buffer` null | copy helper FUN_1800cca70 [P]. |
| 20 | `SetUserData`(const char* key, const char* value, listener? [I]) `0x1800f8c60` | void | UA "not logged on"; IA `key` null/len/empty (len+1 <= 0x400); IA `value` null/len (len+1 <= 0x1000) | value strlen+1 <= 0x1000 [P]; logs "trying to delete data that doesn't exist" / "trying to set the same data" [P strings]. |
| 21 | `GetUserDataCount`(uint64 userID) `0x18014baa0` | uint32; fail: 0 [P] | none at facade level [P] |  |
| 22 | `GetUserDataByIndex`(uint32 index, ptr key, uint32 keyLength, ptr value, uint32 valueLength, listener? [I]) `0x18014b4c0` | bool; fail: false [P] | IA `key` null; IA `value` null |  |
| 23 | `DeleteUserData`(const char* key, listener? [I]) `0x1800f8630` | void | UA "not logged on"; IA `key` null/len/empty (len+1 <= 0x400) |  |
| 24 | `IsLoggedOn`() `0x18014bc90` | bool; fail: false [P] | none at facade level [P] |  |
| 25 | `RequestEncryptedAppTicket`(ptr data, uint32 dataSize, listener? [I]) `0x18014c550` | void | UA "not logged on"; IA `data` size (len+1 <= 0x400) |  |
| 26 | `GetEncryptedAppTicket`(ptr encryptedAppTicket, uint32 maxEncryptedAppTicketSize, uint32 currentEncryptedAppTicketSize) `0x18014adb0` | void | IA `encryptedAppTicket` null; IS "encryptedAppTicket not retrieved" | IS "encryptedAppTicket not retrieved"; copies with binary helper FUN_1800cc9c0 (no terminator); writes *currentEncryptedAppTicketSize = stored size (full size, not the copied size) [P: `*param_4 = (int)*(lVar2+0x10)`]. |
| 27 | `GetSessionID`() `0x18014b050` | uint64 SessionID; fail: 0 [P] | UA "not logged on"; IS "session ID not available yet" | IS "session ID not available yet" [P]. |
| 28 | `GetAccessToken`() `0x18014a970` | const char*; fail: "" (0x1809bc000) [P] | UA "not logged on"; IS "access token not available yet" | IS "access token not available yet"; logs "Current access token: %s" at debug level with the token masked by FUN_18074eeb0(...,4) [P call; masking I]. |
| 29 | `GetAccessTokenCopy`(ptr buffer, uint32 bufferLength) `0x18014ab80` | void | UA "not logged on"; IA `buffer` null; IS "access token not available yet" | As GetAccessToken; copy via FUN_1800cca70 [P]. copy helper FUN_1800cca70 [P]. |
| 30 | `ReportInvalidAccessToken`(const char* accessToken, listener? [I]) `0x18014bf70` | bool; fail: false [P] | IA `accessToken` null/empty; IS "access token not available yet" | Returns true only when the reported token equals the current one (memcmp + length) and then triggers a refresh (FUN_180149db0) [P]; otherwise logs "invalid access token is different than the current one" and returns false [P]. |

## IUtils — `PeerUtilsFacade` vtable `0x1809cb7b0`

| Slot | Method (args) | Returns / on failure | Preconditions -> error | Notes |
|---|---|---|---|---|
| 0 | destructor (adjustor thunk `0x180125ea0`: sub rcx,0x58; jmp 0x180028006) | — | — | [P] |
| 1 | `GetImageSize`(uint32 imageID, +2 more (listener/out params) [I]) `0x1801f68d0` | void | IA "image does not exist" | IA "GetImageSize: image does not exist" [P]; out params uint32* width, uint32* height [I from Ghidra]. |
| 2 | `GetImageRGBA`(uint32 imageID, ptr buffer, uint32 bufferLength) `0x1801f6770` | void | IA `buffer` null; IA "image does not exist" | Same IA message (says GetImageSize) [P]. |
| 3 | `RegisterForNotification`(const char* type) `0x180126800` | void | UA "not logged on"; IA `type` null/len (len+1 <= 0x20); IA "Invalid notification type" | IA "Invalid notification type" [P]; type must not start with "__" [I: "__" literal in body]. |
| 4 | `GetNotification`(uint64 notificationID, uint32 typeLength, uint32 contentSize, +3 more (listener/out params) [I]) `0x180126400` | uint32 bytes; fail: 0 [P] | IA `type` null; IA `content` null | Args (NotificationID, bool* consumable, char* type, uint32 typeLength, void* content, uint32 contentSize) [I from 7 Ghidra params]; type copied with FUN_1800cca70 (terminated), content with FUN_1800cc9c0 (not terminated) [P]. |
| 5 | `ShowOverlayWithWebPage`(const char* url) `0x180126980` | void | UA "not logged on"; UA "not signed in", then IS "not signed in with Galaxy"; IA `url` null/len/empty (len+1 <= 0x800); IS "overlay is not initialized" | IS "overlay is not initialized" [P]. |
| 6 | `IsOverlayVisible`() `0x180126680` | bool; fail: false [P] | IS "overlay is not initialized" | IS "overlay is not initialized" [P]. |
| 7 | `GetOverlayState`() `0x180126580` | enum OverlayState; fail: 0 [P] | none at facade level [P] |  |
| 8 | `DisableOverlayPopups`(listener? [I]) `0x180126130` | void | UA "not signed in", then IS "not signed in with Galaxy"; IA `popupGroup` null/empty; IS "overlay is not initialized"; IA "popupGroup=%s is unknown" | `popupGroup` null/empty; known group literal "chat_message" in body [P]. |
| 9 | `GetGogServicesConnectionState`() `0x1801f6670` | enum GogServicesConnectionState; fail: 0 [P] | none at facade level [P] |  |

## IFriends — `FriendsFacade` vtable `0x1809c4f60`

| Slot | Method (args) | Returns / on failure | Preconditions -> error | Notes |
|---|---|---|---|---|
| 0 | destructor (adjustor thunk `0x1801032cc`: sub rcx,0x58; jmp 0x18003032d) | — | — | [P] |
| 1 | `GetDefaultAvatarCriteria`() `0x180104130` | uint32 AvatarCriteria; fail: 0 [P] | none at facade level [P] |  |
| 2 | `SetDefaultAvatarCriteria`(uint32 defaultAvatarCriteria) `0x180107f10` | void | none at facade level [P] |  |
| 3 | `RequestUserInformation`(uint64 userID, uint32 avatarCriteria, listener? [I]) `0x180107840` | void | UA "not logged on" |  |
| 4 | `IsUserInformationAvailable`(uint64 userID) `0x1801065b0` | bool; fail: false [P] | none at facade level [P] |  |
| 5 | `GetPersonaName`() `0x1801052c0` | const char*; fail: "" (0x1809bc000) [P] | UA "not signed in" (user object) |  |
| 6 | `GetPersonaNameCopy`(ptr buffer, uint32 bufferLength) `0x1801053f0` | void | UA "not signed in" (user object); IA `buffer` null | copy helper FUN_1800cca70 [P]. |
| 7 | `GetPersonaState`() `0x180105540` | enum PersonaState; fail: 0 [P] | none at facade level [P] |  |
| 8 | `GetFriendPersonaName`(uint64 userID) `0x180104de0` | const char*; fail: "" (0x1809bc000) [P] | none at facade level [P] |  |
| 9 | `GetFriendPersonaNameCopy`(uint64 userID, ptr buffer, uint32 bufferLength) `0x180104f40` | void | IA `buffer` null | copy helper FUN_1800cca70 [P]. |
| 10 | `GetFriendPersonaState`(uint64 userID) `0x1801050a0` | enum PersonaState; fail: 0 [P] | none at facade level [P] |  |
| 11 | `GetFriendAvatarUrl`(uint64 userID, int32 avatarType) `0x1801044f0` | const char*; fail: "" (0x1809bc000) [P] | none at facade level [P] |  |
| 12 | `GetFriendAvatarUrlCopy [I]`(uint64 userID, int32 avatarType, ptr buffer, uint32 bufferLength) `0x180104670` | void | IA `buffer` null | Real name GetFriendAvatarUrlCopy [I]; the catch-block log string says "GetFriendAvatarUrl" (GOG copy-paste) [P]. copy helper FUN_1800cca70 [P]. |
| 13 | `GetFriendAvatarImageID`(uint64 userID, uint32 avatarType) `0x180104230` | uint32 imageID; fail: 0 [P] | none at facade level [P] |  |
| 14 | `GetFriendAvatarImageRGBA`(uint64 userID, uint32 avatarType, ptr buffer, uint32 bufferLength) `0x180104380` | void | IA `buffer` null |  |
| 15 | `IsFriendAvatarImageRGBAAvailable`(uint64 userID, uint32 avatarType) `0x180106280` | bool; fail: false [P] | none at facade level [P] |  |
| 16 | `RequestFriendList`(listener? [I]) `0x180107250` | void | UA "not logged on" |  |
| 17 | `IsFriend`(uint64 userID) `0x180106100` | bool; fail: false [P] | UA "not logged on" |  |
| 18 | `GetFriendCount`() `0x1801049c0` | uint32; fail: 0 [P] | UA "not logged on" |  |
| 19 | `GetFriendByIndex`(uint32 index) `0x1801047f0` | GalaxyID (sret); fail: returns the sret pointer (`mov rax,[rsp+0x30]`) [P]; GalaxyID value written on failure not read | UA "not logged on"; IS "friends list not retrieved" | IS "friends list not retrieved", out-of-range index [P strings]. |
| 20 | `SendFriendInvitation`(uint64 userID, listener? [I]) `0x180107c90` | void | UA "not logged on" |  |
| 21 | `RequestFriendInvitationList`(listener? [I]) `0x180107130` | void | UA "not logged on" |  |
| 22 | `RequestSentFriendInvitationList`(listener? [I]) `0x180107720` | void | UA "not logged on" |  |
| 23 | `GetFriendInvitationCount`() `0x180104ca0` | uint32; fail: 0 [P] | UA "not logged on" |  |
| 24 | `GetFriendInvitationByIndex`(uint32 index, +2 more (listener/out params) [I]) `0x180104ad0` | void (out: GalaxyID* userID, uint32* sendTime) | UA "not logged on" |  |
| 25 | `RespondToFriendInvitation`(uint64 userID, uint32 accept, listener? [I]) `0x180107b60` | void | UA "not logged on" |  |
| 26 | `DeleteFriend`(uint64 userID, listener? [I]) `0x180103ac0` | void | UA "not logged on" |  |
| 27 | `SetRichPresence`(const char* key, const char* value, listener? [I]) `0x180108100` | void | UA "not logged on"; UA "not signed in", then IS "not signed in with GOG"; IA `key` null; IA `value` null/len (len+1 <= 0x800); IA `value` null/len (len+1 <= 0xbb9); IA `value` null/len (len+1 <= 0x1000); IA "key=%s is unknown" | Key must be "status" (value strlen+1 <= 0xbb9), "metadata" (<= 0x800) or "connect" (<= 0x1000), else IA "key=%s is unknown"; NULL value is replaced by "" [P: FUN_180108100]. |
| 28 | `DeleteRichPresence`(const char* key, listener? [I]) `0x180103bf0` | void | UA "not logged on"; IA `key` null; IA "key=%s is unknown" | Same key whitelist; the unknown-key message says "SetRichPresence: key=%s is unknown" (copy-paste in GOG code) [P]. |
| 29 | `ClearRichPresence`(listener? [I]) `0x1801037d0` | void | UA "not logged on" | Logs "Trying to clear RichPresence when no presence info is avaiable" [P string]. |
| 30 | `RequestRichPresence`(uint64 userID, listener? [I]) `0x1801074a0` | void | UA "not logged on" |  |
| 31 | `GetRichPresence`(uint64 userID, const char* key) `0x180105640` | const char*; fail: "" (0x1809bc000) [P] | IA `key` null |  |
| 32 | `GetRichPresenceCopy`(uint64 userID, const char* key, ptr buffer, uint32 bufferLength) `0x180105c00` | void | IA `key` null; IA `buffer` null | copy helper FUN_1800cca70 [P]. |
| 33 | `GetRichPresenceCount`(uint64 userID) `0x180105f10` | uint32; fail: 0 [P] | none at facade level [P] |  |
| 34 | `GetRichPresenceByIndex`(uint64 userID, uint32 index, +4 more (listener/out params) [I]) `0x180105940` | void (out: key/value buffers) | IA `key` null; IA `value` null |  |
| 35 | `ShowOverlayInviteDialog`(const char* connectionString) `0x1801086a0` | void | UA "not logged on"; UA "not signed in", then IS "not signed in with Galaxy"; IA `connectionString` null/len/empty (len+1 <= 0x1000); IS "overlay is not initialized" | IS "overlay is not initialized" [P]. |
| 36 | `SendInvitation`(uint64 userID, const char* connectionString, listener? [I]) `0x180107dc0` | void | UA "not logged on"; IA `connectionString` null/len/empty (len+1 <= 0x1000) |  |
| 37 | `FindUser`(const char* userSpecifier, listener? [I]) `0x180104000` | void | UA "not logged on"; IA `userSpecifier` null/len/empty (len+1 <= 0x400) |  |
| 38 | `IsUserInTheSameGame`(uint64 userID) `0x1801063d0` | bool; fail: false [P] | UA "not logged on" |  |

## IStorage — `StorageFacade` vtable `0x1809cc238`

| Slot | Method (args) | Returns / on failure | Preconditions -> error | Notes |
|---|---|---|---|---|
| 0 | destructor (adjustor thunk `0x180127e20`: sub rcx,0x58; jmp 0x18002ecc1) | — | — | [P] |
| 1 | `FileWrite`(const char* fileName, ptr data, uint32 dataSize) `0x1801287b0` | void | IS "file sharing is in progress" | IS "file sharing is in progress"; write goes through a storage object vtable (+0x8 region) from FUN_1801290d0 [P call; slot meaning I]. No facade-level fileName check [P: none called]. |
| 2 | `FileRead`(const char* fileName, ptr data, uint32 dataSize) `0x1801282c0` | uint32 bytes read; fail: 0 [P] | none at facade level [P] | No facade precondition helpers; vtable call +0x10 on the storage object [P]. |
| 3 | `FileDelete`(const char* fileName) `0x180128040` | void | IS "file sharing is in progress" | IS "file sharing is in progress" [P]. |
| 4 | `FileExists`(const char* fileName) `0x180128190` | bool; fail: false [P] | IA `fileName` null/empty |  |
| 5 | `GetFileSize`(const char* fileName) `0x180128fa0` | uint32; fail: 0 [P] | IA `fileName` null/empty |  |
| 6 | `GetFileTimestamp`(const char* fileName) `0x180129180` | uint32; fail: 0 [P] | IA `fileName` null/empty |  |
| 7 | `GetFileCount`() `0x180128b40` | uint32; fail: 0 [P] | none at facade level [P] |  |
| 8 | `GetFileNameByIndex`(uint32 index) `0x180128c40` | const char*; fail: "" (0x1809bc000) [P] | none at facade level [P] |  |
| 9 | `GetFileNameCopyByIndex`(uint32 index, ptr buffer, uint32 bufferLength) `0x180128d60` | void | IA `buffer` null | No copy at facade level: forwards to storage object vtable +0x48 (index, buffer, bufferLength) [P]; terminator behaviour not read.  |
| 10 | `FileShare`(const char* fileName, listener? [I]) `0x1801283e0` | void | UA "not signed in"; IA `fileName` null/empty; IA "file does not exist"; IS "file is already being shared" | IS "file does not exist" / "file is already being shared" [P strings]. |
| 11 | `DownloadSharedFile`(uint64 sharedFileID, listener? [I]) `0x180127f40` | void | UA "not signed in" |  |
| 12 | `GetSharedFileName`(uint64 sharedFileID) `0x1801292b0` | const char*; fail: "" (0x1809bc000) [P] | UA "not signed in" |  |
| 13 | `GetSharedFileNameCopy`(uint64 sharedFileID, ptr buffer, uint32 bufferLength) `0x180129400` | void | UA "not signed in"; IA `buffer` null | copy helper FUN_1800cca70 [P]. |
| 14 | `GetSharedFileSize`(uint64 sharedFileID) `0x180129690` | uint32; fail: 0 [P] | UA "not signed in" |  |
| 15 | `GetSharedFileOwner`(uint64 sharedFileID) `0x180129550` | GalaxyID (sret); fail: returns the sret pointer (`mov rax,[rsp+0x30]`) [P]; GalaxyID value written on failure not read | UA "not signed in" |  |
| 16 | `SharedFileRead`(uint64 sharedFileID, ptr data, uint32 dataSize, uint32 offset) `0x180129ae0` | uint32 bytes read; fail: 0 [P] | UA "not signed in"; IA `data` null | IS "given offset is invalid"; copy via FUN_1800cc9f0 (min(size,dataSize), no terminator) [P]. |
| 17 | `SharedFileClose`(uint64 sharedFileID) `0x1801299d0` | void | UA "not signed in" |  |
| 18 | `GetDownloadedSharedFileCount`() `0x180128a30` | uint32; fail: 0 [P] | UA "not signed in" |  |
| 19 | `GetDownloadedSharedFileByIndex`(uint32 index) `0x180128910` | uint64 SharedFileID; fail: 0 [P] | UA "not signed in" | Catch-block log string says "GetDownloadedSharedFileCount" (GOG copy-paste) [P]; body string is "GetDownloadedSharedFileByIndex" [P]. |

## Not read / open

- Internal semantics behind the facade (what FUN_1800e0320, the storage object vtable, or the listener dispatch do) were not followed.
- Return types for enum/struct-valued getters are named from SDK convention [I]; widths are proven only as 32-bit (eax) or 8-bit (al).
- Out-parameter layout of GetAchievement, GetRequestedLeaderboardEntry*, GetFriendInvitationByIndex, GetImageSize, GetNotification is from Ghidra parameter counts, not from reading each store [I].
- Conditions of some IS/UA throws (e.g. "requesting other user's data") are read as strings; the guarding condition was not decoded [I].
