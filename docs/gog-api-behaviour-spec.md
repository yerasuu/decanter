# GOG API client: behaviour specification for the clean-room rewrite

Step 1 of the clean-room rewrite of kb #1227 (decision #1225). Four files of `crates/gog/src/api/client/`
(`auth.rs`, `user.rs`, `products.rs`, `achievements.rs`) derived from gog 0.5.0 (GPL-3.0-only); they were
rewritten from this document (kb #1228) and no derived code remains. It says what they must do so that they can
be written again from scratch, without reading them, and released under GPL-3.0-or-later with the rest of the
crate.

Whoever writes the new files reads this document, the public types of the callers named here, and decanter's
own files of the same directory (`mod.rs`, `http.rs`, `error.rs` and the other modules, all GPL-3.0-or-later).
They never read the four files being replaced, nor their history in git.

Facts marked *(from the current implementation)* could not be confirmed in a public source; everything else
cites one.

## Contents

1. Ground rules for the new files
2. Shared machinery the four modules use (decanter's own `http.rs` and `error.rs`)
3. `auth`: the session token, the login code exchange and the refresh
4. `user`: the account and the owned product ids
5. `products`: the filtered products listing
6. `achievements`: one game's achievements
7. Token lifecycle across the client
8. Security rules
9. Sources

## 1. Ground rules for the new files

- Each new file starts with `// SPDX-License-Identifier: GPL-3.0-or-later` and has no based-on line.
- The module names stay `auth`, `user`, `products` and `achievements`, declared privately in
  `client/mod.rs`, which re-exports (unchanged):
  - from `auth`: `CLIENT_ID`, `CLIENT_SECRET`, `REDIRECT_URI`, `Token`;
  - from `user`: `User`;
  - from `products`: `ProductDetails`, `ProductsPage`;
  - from `achievements`: `Achievement`.
- `http.rs` must compile unchanged against the new `auth` (section 2.3 lists what it calls).
- The workspace's rules apply: no `unwrap`/`expect`/`panic!` on reachable paths, clippy `all`, `pedantic`,
  `restriction` and `nursery` at warn with the workspace deny list, `cargo fmt` at 120 columns, comments only
  where they explain why. No tests (repository rule).
- Endpoints, parameter names, JSON field names and status codes are GOG's and must be exactly as given here.

## 2. Shared machinery the four modules use

These items are decanter's own (`client/http.rs`, `client/error.rs`) and stay as they are. The four modules
plug into them.

### 2.1 `Error` (`client/error.rs`)

`#[non_exhaustive] enum Error` with the variants `Rejected { call, reason }`, `Network { call, source }`,
`Unavailable { call, status }`, `Malformed { call, reason }`, `InvalidInput { what }` and `Setup(source)`.
`call` and `what` are `&'static str`; `reason` is `String`. The meaning the four modules must respect:

| Variant | Means | Produced by |
| --- | --- | --- |
| `Rejected` | GOG turned the credentials down; only a new browser login helps | the shared HTTP layer for a status in the call's refused list (reason `HTTP <code>`); `user` for a "not logged in" answer |
| `Network` | no answer (offline, DNS, TLS, timeout, redirect off GOG) | the shared HTTP layer only, always with the URL stripped from the source error |
| `Unavailable` | an answer whose status says nothing about the credentials (5xx, 404, 429...) | the shared HTTP layer only |
| `Malformed` | a 2xx answer too large or not of the expected shape | the shared HTTP layer (size, JSON); `auth` and `user` for a bad account id |
| `InvalidInput` | input refused before anything was sent | `auth` (login code), `achievements` (product id, stored account id) |
| `Setup` | the HTTP client could not be built | `http.rs` only |

The caller in `crates/gog/src/session.rs` turns `Rejected` into a closed session (the user must log in again),
`Network`, `Unavailable` and `Malformed` into "GOG unreachable" (nothing changes on screen), and `InvalidInput`
and `Setup` into a local error. So a module must never report a transport or parsing problem as `Rejected`, and
must report a credentials refusal only as `Rejected`.

### 2.2 What `http.rs` offers the modules

- `Host`: `Copy` enum of GOG hosts; the four modules use `Host::Auth` (`auth.gog.com`), `Host::Embed`
  (`embed.gog.com`) and `Host::Gameplay` (`gameplay.gog.com`). `Host::url(path)` gives `https://<host><path>`.
- `SESSION_REFUSED: &[StatusCode]`: 401 and 403, the refused list for authorized calls.
- `Call`: a request kind, a struct with public fields `name: &'static str` (its label in errors),
  `refused: &'static [StatusCode]`, `max_body: usize` (byte cap of the answer) and `has_secrets: bool` (when
  true a JSON parse error reports only line and column and quotes nothing of the body). Each module declares its
  own `Call` constants.
- `send::<T>(request: reqwest::RequestBuilder, call: &Call) -> Result<T, Error>`: sends, maps the status
  (refused list to `Rejected`, other non-2xx to `Unavailable`), reads at most `max_body` bytes (more is
  `Malformed`, checked on the declared length and while streaming), and parses JSON as `T` (failure is
  `Malformed`). Used by `auth` for the unauthenticated token requests.
- `Client::get(&self, call, host, path, query: &[(&str, &str)], accept)` where
  `accept: impl Fn(T) -> Result<U, Error>` and `T: DeserializeOwned`: an authorized GET that
  - takes a valid access token (refreshing first when `Token::is_expired()` says so);
  - sends `Authorization: Bearer <access token>` and the header `CSRF: csrf=true` *(from the current
    implementation; the community docs mention only the bearer header,
    [gogapidocs: Authentication](https://gogapidocs.readthedocs.io/en/latest/auth.html))*;
  - passes the parsed body to `accept`;
  - when the send or `accept` ends in `Rejected` and the token was not renewed during this call, renews once and
    tries once more (so a `Rejected` returned from `accept` also triggers the retry).
- `Client::token(&self) -> Token` (async): a clone of the current session.
- Every request has a 30 second total timeout, is HTTPS only, and follows at most 5 redirects, only to HTTPS URLs
  on one of the `Host` names; any other redirect is an error (`Network`).

### 2.3 What `http.rs` calls in `auth` (must exist with these shapes)

- `Token`, with public fields `access_token: String` and `refresh_token: String`, `Clone`, and
  `fn is_expired(&self) -> bool`.
- `pub(super) async fn exchange_code(http: &reqwest::Client, code: &str) -> Result<Token, Error>`
- `pub(super) async fn refresh(http: &reqwest::Client, refresh_token: &str) -> Result<Token, Error>`

`user` and `achievements` also need an account id check; in the current tree it is a `pub(super)` function of
`auth`, `is_account_id(&str) -> bool`, which `http.rs` does not use. Its name and place are free.

## 3. `auth`

### 3.1 Public constants

| Constant | Type | Value |
| --- | --- | --- |
| `CLIENT_ID` | `&str` | `46899977096215655`, GOG Galaxy's client id ([gogapidocs: Authentication](https://gogapidocs.readthedocs.io/en/latest/auth.html)) |
| `CLIENT_SECRET` | `&str` | GOG Galaxy's client secret, the value published on the same page; public, but never logged nor quoted in an error |
| `REDIRECT_URI` | `&str` | `https://embed.gog.com/on_login_success?origin=client` ([gogapidocs: Authentication](https://gogapidocs.readthedocs.io/en/latest/auth.html)) |

`crates/gog/src/auth.rs` builds the browser login URL from `CLIENT_ID` and `REDIRECT_URI`
(`https://auth.gog.com/auth?client_id=…&redirect_uri=<percent-encoded>&response_type=code&layout=client2`), so
both must be exactly these strings.

### 3.2 `Token`: the stored session, byte-compatible

`pub struct Token`, derives `Clone`, `serde::Serialize`, `serde::Deserialize`, and nothing else. In particular no
`Debug` and no `Display`, so that a token cannot end up in a log by formatting. All fields `pub`, declared in
this order, which is also the serialization order:

| # | Field | Rust type | JSON | Serde attributes |
| --- | --- | --- | --- | --- |
| 1 | `expires_in` | `u64` | number, seconds from `updated_at` to expiry | none |
| 2 | `scope` | `String` | string (GOG sends `""`) | none |
| 3 | `token_type` | `String` | string (GOG sends `"bearer"`) | none |
| 4 | `access_token` | `String` | string | none |
| 5 | `user_id` | `String` | string of decimal digits (the account id) | none |
| 6 | `refresh_token` | `String` | string | none |
| 7 | `session_id` | `String` | string | none |
| 8 | `updated_at` | `u64` | number, Unix seconds when the token was received | when absent, defaults to the current Unix time at deserialization |

No `rename`, no `rename_all`, no `skip_serializing_if`, no `deny_unknown_fields`: every field is always written,
unknown fields are ignored on read, and the first seven are required. The first seven are exactly the fields of
GOG's token answer ([gogapidocs: Authentication](https://gogapidocs.readthedocs.io/en/latest/auth.html) shows
`expires_in`, `scope`, `token_type`, `access_token`, `user_id`, `refresh_token`, `session_id`), so the same type
deserializes GOG's answer and the stored session.

`updated_at` is not in GOG's answer, so the default stamps a token on receipt; a stored session written without
it (if any) reads as received now. The current Unix time is the seconds since the epoch of the system clock, and
0 if the clock is before the epoch.

The keyring holds `serde_json::to_string(&token)` (compact JSON, fields in the order above),
for example (placeholder values):

```json
{"expires_in":3600,"scope":"","token_type":"bearer","access_token":"A","user_id":"48628349957132247","refresh_token":"R","session_id":"6354900816570477251","updated_at":1761700000}
```

`crates/gog/src/session.rs` compares this string with the stored one to decide whether to write the keyring
again, so a change of field order or a skipped field would rewrite every stored session; a missing or renamed
field would log everyone out. `crates/core/src/credentials.rs` stores the string as it is.

`is_expired(&self) -> bool`: true when `updated_at + expires_in` (saturating) is less than or equal to the
current Unix time; no safety margin *(from the current implementation)*. `#[inline]` and `#[must_use]` as the
workspace lints ask.

### 3.3 Login code exchange

Input: the one-shot code from GOG's login redirect (callers extract it from the pasted URL first).

Validated before anything is sent, otherwise `Error::InvalidInput { what: "login code" }`:

- not empty and at most 2048 bytes;
- every byte an ASCII letter, an ASCII digit, or one of `-`, `_`, `.`, `~` (URL-unreserved characters).

*(Both limits are from the current implementation.)*

Request ([gogapidocs: Authentication](https://gogapidocs.readthedocs.io/en/latest/auth.html)):

- `GET https://auth.gog.com/token`, no `Authorization` header, sent through the shared `send`;
- query parameters, URL-encoded by reqwest, in this order: `client_id` = `CLIENT_ID`,
  `client_secret` = `CLIENT_SECRET`, `grant_type` = `authorization_code`, `redirect_uri` = `REDIRECT_URI`,
  `layout` = `client2`, `code` = the code. `layout` on this request is *from the current implementation* (the
  docs list it only on the login page).

Answer handling:

- call label `"login code exchange"`, refused statuses 400, 401 and 403 (GOG answers a used or expired code with
  400, *from the current implementation*), body cap 64 KiB, `has_secrets` true (the body holds tokens);
- the answer is read as a `Token`, with `updated_at` defaulted to now;
- `user_id` must be an account id (section 3.5), otherwise
  `Error::Malformed { call: "login code exchange", reason: "user_id is not a numeric id" }`.

### 3.4 Refresh

Input: the current refresh token. It is not validated before sending *(from the current implementation)*.

Request ([gogapidocs: Authentication](https://gogapidocs.readthedocs.io/en/latest/auth.html)):

- `GET https://auth.gog.com/token`, no `Authorization` header, through `send`;
- query, in this order: `client_id`, `client_secret`, `grant_type` = `refresh_token`,
  `refresh_token` = the refresh token. No `redirect_uri`, `layout` or `without_new_session`.

Answer handling: as the exchange, with call label `"token refresh"` (refused 400, 401, 403; 64 KiB;
`has_secrets` true; the same `user_id` check and `Malformed` reason). The new `Token` replaces the old one
whole, including its refresh token: GOG rotates the refresh token, so the old one is spent once the refresh
succeeds *(from the current implementation; the community docs only say to renew after about an hour and do not
state rotation)*.

### 3.5 Account id

A GOG account id is accepted when it is 1 to 20 bytes, all ASCII digits *(from the current implementation; the
docs show ids such as `48628349957132247`, 17 digits, and a `u64` has at most 20)*. It is checked because it
becomes a keyring entry name, a database key and a path segment.

## 4. `user`

### 4.1 `User`

`#[non_exhaustive] pub struct User` with `pub id: String` (the account id, decimal digits) and
`pub username: String`. No derives. Callers (`crates/gog/src/session.rs`, `crates/gog/src/auth.rs`) read both
fields by move.

### 4.2 `Client::user(&self) -> Result<User, Error>` (async, `#[inline]`)

The account behind the session; it also serves as the session check, so its error kinds matter.

Request ([gogapidocs: Account](https://gogapidocs.readthedocs.io/en/latest/account.html)): authorized
`GET https://embed.gog.com/userData.json`, no query, through `Client::get`. Call label `"user data"`, refused
401 and 403, cap 2 MiB, `has_secrets` true because the body holds personal data (e-mail and others).

Fields read (camelCase JSON; all others ignored):

| JSON | Type | Required |
| --- | --- | --- |
| `isLoggedIn` | bool | yes (missing is `Malformed` from the parse) |
| `userId` | string | no |
| `username` | string | no |

Outcome:

- `isLoggedIn` false: `Error::Rejected { call: "user data", reason: "not logged in" }`. Because it is
  `Rejected` and comes out of `accept`, `Client::get` refreshes once and asks again before giving up.
- `isLoggedIn` true with both `userId` (an account id, section 3.5) and `username` present:
  `User { id: userId, username }`.
- Otherwise `Error::Malformed { call: "user data", reason: "no numeric user id or no username" }`.

The docs show `userId` as a string (`"48628349971017"`), [gogapidocs: Account](https://gogapidocs.readthedocs.io/en/latest/account.html).

### 4.3 `Client::owned_games(&self) -> Result<Vec<i64>, Error>` (async, `#[inline]`)

The product ids of everything the account owns.

Request ([gogapidocs: Account](https://gogapidocs.readthedocs.io/en/latest/account.html)): authorized
`GET https://embed.gog.com/user/data/games`, no query. Call label `"owned games"`, refused 401 and 403, cap
2 MiB, `has_secrets` false.

Answer: an object whose `owned` field (required) is an array of integers; return it as it is, in GOG's order.

## 5. `products`

### 5.1 `ProductDetails`

`#[non_exhaustive] pub struct ProductDetails`, derives `serde::Deserialize` only, JSON names camelCase. Only the
fields decanter reads; unknown fields are ignored, so a product lacking an unread field still parses. All five are
required:

| Field | Type | JSON |
| --- | --- | --- |
| `id` | `i64` | `id` |
| `title` | `String` | `title` |
| `image` | `String` | `image`: protocol-relative base URL without size suffix or extension, e.g. `//images-2.gog.com/<hash>` |
| `is_game` | `bool` | `isGame` |
| `is_movie` | `bool` | `isMovie` |

Shapes from [gogapidocs: Listing](https://gogapidocs.readthedocs.io/en/latest/listing.html).
`crates/gog/src/library/fetch.rs` turns `image` into a URL itself (`https://`, the base, a size suffix such as
`_196` or `_392`, then `.jpg`), so the field is returned untouched.

### 5.2 `ProductsPage`

`#[non_exhaustive] pub struct ProductsPage`, derives `serde::Deserialize`, camelCase:

| Field | Type | JSON | Missing |
| --- | --- | --- | --- |
| `products` | `Vec<ProductDetails>` | `products` | empty |
| `total_pages` | `u32` | `totalPages` | 0 |

### 5.3 `Client::filtered_products(&self, page: core::num::NonZeroU32) -> Result<ProductsPage, Error>`

(async, `#[inline]`). One page of the owned games, most recently purchased first.

Request ([gogapidocs: Listing](https://gogapidocs.readthedocs.io/en/latest/listing.html)): authorized
`GET https://embed.gog.com/account/getFilteredProducts`, query in this order: `mediaType` = `1`,
`sortBy` = `date_purchased`, `page` = the page number in decimal. Call label `"filtered products"`, refused 401
and 403, cap 4 MiB, `has_secrets` false. The answer is returned as parsed.

The docs show `mediaType=1` and `sortBy: "date_purchased"` in their example but do not define the values;
`mediaType=1` meaning games and `date_purchased` meaning newest purchase first are *from the current
implementation* and the caller's comments. Pagination is the caller's (`library/fetch.rs` starts at page 1 and
stops when `page >= max(total_pages, 1)`); products filed under no media type appear only in `owned_games`.

## 6. `achievements`

### 6.1 `Achievement`

`#[non_exhaustive] pub struct Achievement`, derives `serde::Deserialize` only, JSON names as the field names
(snake_case, no renaming). Unknown fields ignored. Fields:

| Field | Type | Required |
| --- | --- | --- |
| `achievement_key` | `String` | yes |
| `name` | `String` | yes |
| `description` | `String` | yes |
| `image_url_unlocked` | `String` | yes |
| `image_url_locked` | `String` | yes |
| `date_unlocked` | `Option<String>` | `null` or absent while locked |

`date_unlocked` is kept as GOG writes it, e.g. `2021-10-12T00:00:56+0000` (format observed by decanter's app,
`app/src/views/library/data.rs`; the docs show only `null`). `crates/gog/src/lib.rs::poll_achievements` moves
every field out and treats `date_unlocked.is_some()` as unlocked. GOG's answer also carries `achievement_id` and
`visible`, which are not read. Field list from [gogapidocs: Galaxy](https://gogapidocs.readthedocs.io/en/latest/galaxy.html).

### 6.2 `Client::achievements(&self, product_id: i64) -> Result<Vec<Achievement>, Error>`

(async, `#[inline]`). Every achievement of the product with this account's progress.

Checks before anything is sent:

1. `product_id <= 0`: `Error::InvalidInput { what: "product id" }`.
2. The account id is the `user_id` of the client's current token (`Client::token`); if it is not an account id
   (section 3.5): `Error::InvalidInput { what: "stored account id" }`.

Request ([gogapidocs: Galaxy](https://gogapidocs.readthedocs.io/en/latest/galaxy.html)): authorized
`GET https://gameplay.gog.com/clients/<product_id>/users/<user_id>/achievements`, ids in decimal, no query.
Call label `"achievements"`, refused 401 and 403, cap 4 MiB, `has_secrets` false.

Answer: an object whose `items` array (required) is the list returned, in GOG's order. `total_count`, `limit`
and `page_token` are ignored; the endpoint answers the whole list in one page (the docs' example has
`limit` 1000), so no paging is done *(the single-page assumption is from the current implementation)*.

## 7. Token lifecycle across the client

1. **Login.** `Client::from_login_code` (in `http.rs`) calls `exchange_code`; the new `Token` is stamped
   `updated_at = now`. `crates/gog/src/auth.rs` then calls `Client::user()` and stores `Client::token()` under the
   account id it returned.
2. **Use.** Every authorized call first asks whether the token has expired (`is_expired`, section 3.2); if so it
   refreshes before sending.
3. **Refresh.** One request to `auth.gog.com/token` with the refresh grant (section 3.4); the whole token is
   replaced, the new refresh token included. A refusal (400, 401, 403) is `Rejected`; a 5xx or a timeout is
   `Unavailable` or `Network`, which leaves the stored session as it was.
4. **Retry once.** An authorized call answered 401 or 403 (or `userData.json` saying not logged in) with a token
   that was not renewed during this call is retried once after a refresh; a second refusal is the answer. A call
   that refreshed because the token had expired is not retried.
5. **One refresh at a time.** The token sits behind one async lock in `Client` (`http.rs`): a refresh takes the
   write lock, and does nothing if the access token already differs from the one the caller saw, because a
   concurrent caller has renewed it. A second refresh with the same refresh token would spend a token GOG has
   already rotated, which GOG reads as a revoked session. Across clients, `crates/gog/src/session.rs` holds a
   per-account mutex while loading, using and storing a session.
6. **Store.** After every call, failed ones included, `session.rs` serializes `Client::token()` and writes it to
   the keyring when it differs from what was stored, since a refresh may succeed before the request that then
   fails. The new `auth` must therefore keep the JSON exact (section 3.2).

## 8. Security rules

- **Nothing secret in logs or errors.** The client secret, access and refresh tokens, the login code and the
  user's personal data never appear in a log line or an error message. Concretely:
  - `Token` has no `Debug` or `Display`;
  - `Error::Network` always carries the reqwest error without its URL, since the token URLs carry the client
    secret, the refresh token or the login code in the query (`http.rs` does this when the shared `send` is used;
    any new direct use of reqwest must do the same);
  - the token calls and `userData.json` set `has_secrets`, so a parse error reports only line and column;
  - `Rejected` reasons are fixed strings or `HTTP <code>`; no body text.
- **Validated before sending:** the login code (section 3.3), the product id (positive) and the stored account id
  (section 3.5) for achievements. Account ids that come back from GOG (token answers, `userData.json`) are
  validated before they are returned, because they become keyring names, database keys and URL path segments.
- **Hosts.** Only `auth.gog.com`, `embed.gog.com` and `gameplay.gog.com` for these modules, over HTTPS; redirects
  only to HTTPS GOG hosts of the `Host` list and at most 5 (enforced by `http.rs`), since the bearer token rides
  along.
- **Bounded answers.** Every answer has a byte cap (64 KiB for token answers, 2 MiB for user data and owned
  games, 4 MiB for filtered products and achievements) and the 30 s timeout.
- **Status codes read apart.** Only the refused statuses become `Rejected`; never map a 5xx, 404, 429, a timeout
  or an unreadable body to `Rejected`, since that would log the user out.

## 9. Sources

- [gogapidocs: Authentication](https://gogapidocs.readthedocs.io/en/latest/auth.html): login page, token
  endpoint, grants, client id and secret, redirect URI, token answer fields, bearer header.
- [gogapidocs: Account](https://gogapidocs.readthedocs.io/en/latest/account.html): `userData.json`,
  `/user/data/games`.
- [gogapidocs: Listing](https://gogapidocs.readthedocs.io/en/latest/listing.html):
  `/account/getFilteredProducts`, its parameters and answer.
- [gogapidocs: Galaxy](https://gogapidocs.readthedocs.io/en/latest/galaxy.html): the `gameplay.gog.com`
  achievements endpoint and its fields.
- GOG publishes no official documentation for these endpoints; the pages above are community documentation.
