// SPDX-License-Identifier: GPL-3.0-or-later
//! One connection's requests (plan section 2). Every request gets an answer, `NOT_IMPLEMENTED`
//! included, unlike comet, which leaves the SDK waiting on its own timeout (gap 1.1).

use core::net::SocketAddr;
use core::num::NonZeroU32;
use core::time::Duration;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use prost::Message as _;
use tokio::task::spawn_blocking;

use crate::codec::Frame;
use crate::header::ResponseHeader;
use crate::host::{GameSlot, Registered, Registry};
use crate::limits::{MAX_CONNECTIONS_PER_GAME, MAX_DESCRIPTION_BYTES, Refusal, Refusals, retry_after_secs};
use crate::proto::communication::{
    AuthInfoRequest, AuthInfoResponse, ClearUserAchievementRequest, EnvironmentType, GetUserAchievementsRequest,
    GetUserAchievementsResponse, GetUserStatsRequest, GetUserStatsResponse, GetUserTimePlayedRequest,
    GetUserTimePlayedResponse, LibraryInfoRequest, LibraryInfoResponse, MessageType, Region,
    UnlockUserAchievementRequest, UpdateStatus, UpdateUserStatRequest, ValueType,
    get_user_achievements_response::UserAchievement, get_user_stats_response::UserStat,
};
use crate::proto::pb::Header;
use crate::proto::pb::response::Status;
use crate::proto::webbroker;
use crate::source::{AchievementList, ClientId, GameStore, SourceError, Stat, StatKind, StatValue};

const COMMUNICATION_SORT: u32 = 1;
const WEBBROKER_SORT: u32 = 2;
/// A client sending ever new `(sort, type)` pairs would otherwise grow a connection's counts
/// without bound.
const MAX_COUNTED_KINDS: usize = 64;
/// The sort and type every pair past [`MAX_COUNTED_KINDS`] is counted under.
pub(crate) const OTHER_KINDS: u32 = u32::MAX;
/// decanter's own login client (`crates/gog/src/api/client/auth.rs`): never a game, so it never
/// signs in here (plan 1.5).
const GALAXY_CLIENT_ID: &str = "46899977096215655";
/// GOG's client secrets are 64 hex digits; the cap leaves room without letting one fill a frame.
const MAX_SECRET_BYTES: usize = 128;
/// A Steam session ticket in hex is about two kilobytes. It is never read, only bounded.
const MAX_TICKET_BYTES: usize = 4096;
/// Plan 2.4; GOG games have far fewer.
const MAX_ACHIEVEMENTS: usize = 4096;
const MAX_STATS: usize = 4096;
/// An unlock time further ahead than this is a wrong clock, not a time, and gets now instead
/// (plan 2.1, row 25).
const MAX_CLOCK_AHEAD_SECONDS: u32 = 24 * 60 * 60;
/// UNVERIFIED: the unit of `GetUserTimePlayedResponse.time_played`. Seconds per the SDK's
/// `GetUserTimePlayed` and decanter's own `playtime.seconds_total`; comet forwards GOG's
/// `time_sum`, whose unit neither comet nor GOG states (plan 2.1 row 43, section 9). The first
/// real-game run checks the number the game shows; if GOG counts minutes this becomes 60.
const TIME_PLAYED_SECONDS_PER_UNIT: u64 = 1;
/// An MSVC toolset number or a Clang version; comet builds a path from it, decanter only logs it.
const MAX_COMPILER_VERSION_BYTES: usize = 64;
/// UNVERIFIED: what the SDK does with `LibraryInfoResponse.location`. comet names the `GalaxyPeer`
/// it downloaded, `<data>/redist/peer/msvc-<version>` (gap G10). decanter downloads no peer, so it
/// names no path at all rather than one where nothing is, and leaks no host path into the prefix.
/// The next real run tells whether the SDK goes on to `AUTH_INFO` with it; if it does not, the
/// alternatives are a peer fetched by decanter (G10) or `UPDATE_FAILED`.
const LIBRARY_LOCATION: &str = "";
/// Topics are short names (friends, chat); plan 2.2.
const MAX_TOPIC_BYTES: usize = 128;

struct Reply {
    code: Status,
    description: Option<String>,
    payload: Vec<u8>,
    retry_after: Option<u32>,
}

impl Reply {
    fn ok(message: &impl prost::Message) -> Self {
        Self {
            code: Status::Ok,
            description: None,
            payload: message.encode_to_vec(),
            retry_after: None,
        }
    }

    /// The protocol has no response message for writes: the header's code is the answer.
    const fn done() -> Self {
        Self {
            code: Status::Ok,
            description: None,
            payload: Vec::new(),
            retry_after: None,
        }
    }

    fn refuse(code: Status, description: impl Into<String>) -> Self {
        Self {
            code,
            description: Some(description.into()),
            payload: Vec::new(),
            retry_after: None,
        }
    }

    fn busy(wait: Duration, description: &str) -> Self {
        Self {
            retry_after: Some(retry_after_secs(wait)),
            ..Self::refuse(Status::Unavailable, description)
        }
    }
}

pub(crate) struct Connection {
    peer: SocketAddr,
    registry: Arc<Registry>,
    /// Bound by the first accepted `AUTH_INFO` and never rebound (comet does the same, gap 2).
    /// Looked up in the registry on every request, so that a game released at exit is refused.
    signed_in: Option<ClientId>,
    /// Held from the first accepted `AUTH_INFO` to the end of the connection.
    slot: Option<GameSlot>,
    refusals: Arc<Refusals>,
    next_oseq: u32,
    /// `(sort, type, status code)` to how many times, logged when the connection ends; at most
    /// [`MAX_COUNTED_KINDS`] entries, the rest under [`OTHER_KINDS`].
    counts: BTreeMap<(u32, u32, i32), u32>,
}

impl Connection {
    pub(crate) const fn new(peer: SocketAddr, registry: Arc<Registry>, refusals: Arc<Refusals>) -> Self {
        Self {
            peer,
            registry,
            signed_in: None,
            slot: None,
            refusals,
            next_oseq: 1,
            counts: BTreeMap::new(),
        }
    }

    pub(crate) const fn counts(&self) -> &BTreeMap<(u32, u32, i32), u32> {
        &self.counts
    }

    pub(crate) const fn is_signed_in(&self) -> bool {
        self.signed_in.is_some()
    }

    pub(crate) async fn handle(&mut self, frame: &Frame<Header>) -> (ResponseHeader, Vec<u8>) {
        let message = sort_one(frame.header.sort, frame.header.r#type);
        let reply = match message {
            Some(MessageType::LibraryInfoRequest) => self.library_info(&frame.payload),
            Some(MessageType::AuthInfoRequest) => self.auth_info(&frame.payload),
            Some(MessageType::GetUserAchievementsRequest) => self.user_achievements(&frame.payload).await,
            Some(MessageType::UnlockUserAchievementRequest) => self.unlock_achievement(&frame.payload).await,
            Some(MessageType::ClearUserAchievementRequest) => self.clear_achievement(&frame.payload).await,
            Some(MessageType::GetUserStatsRequest) => self.user_stats(&frame.payload).await,
            Some(MessageType::UpdateUserStatRequest) => self.update_user_stat(&frame.payload).await,
            Some(MessageType::GetUserTimePlayedRequest) => self.time_played(&frame.payload).await,
            None if webbroker_type(frame.header.sort, frame.header.r#type)
                == Some(webbroker::MessageType::SubscribeTopicRequest) =>
            {
                self.subscribe_topic(&frame.payload)
            }
            _ => Reply::refuse(
                Status::NotImplemented,
                "not implemented by decanter's local Galaxy service",
            ),
        };
        self.respond(&frame.header, message, reply)
    }

    /// The answer to a request over the connection's rate, left unhandled: it still gets its
    /// answer in order, and the SDK is told when to try again.
    pub(crate) fn rate_limited(&mut self, header: &Header, wait: Duration) -> (ResponseHeader, Vec<u8>) {
        let message = sort_one(header.sort, header.r#type);
        self.respond(
            header,
            message,
            Reply::busy(wait, "too many requests on this connection"),
        )
    }

    fn respond(&mut self, request: &Header, message: Option<MessageType>, reply: Reply) -> (ResponseHeader, Vec<u8>) {
        let Header { sort, r#type, oseq, .. } = *request;
        let code = i32::from(reply.code);
        let mut key = (sort, r#type, code);
        if self.counts.len() >= MAX_COUNTED_KINDS && !self.counts.contains_key(&key) {
            key = (OTHER_KINDS, OTHER_KINDS, code);
        }
        let count = self.counts.entry(key).or_default();
        *count = count.saturating_add(1);
        if reply.code == Status::NotImplemented {
            let known = message.map_or_else(
                || webbroker_type(sort, r#type).map(|webbroker| webbroker.as_str_name()),
                |communication| Some(communication.as_str_name()),
            );
            match known {
                Some(name) if *count == 1 => {
                    tracing::warn!(peer = %self.peer, sort, r#type, name, "Galaxy request not implemented, answered 501");
                }
                Some(_) => {}
                None => {
                    if let Some(total) = self.refusals.record(Refusal::UnknownRequest) {
                        tracing::warn!(peer = %self.peer, sort, r#type, total, "unknown Galaxy request, answered 501");
                    }
                }
            }
        }
        let header = ResponseHeader {
            sort,
            r#type: response_type(sort, r#type, message),
            size: None,
            oseq: Some(self.next_oseq),
            rseq: oseq,
            code: Some(code),
            description: reply.description.map(bounded),
            retry_after: reply.retry_after,
        };
        self.next_oseq = self.next_oseq.wrapping_add(1);
        (header, reply.payload)
    }

    /// Served before sign-in because the SDK asks it first and waits for it: a real run sent it 63
    /// times and never reached `AUTH_INFO` while it was refused with 501 (kb #1552). The
    /// connection is still bound by the unauthenticated cap, deadline and rate.
    fn library_info(&self, payload: &[u8]) -> Reply {
        let request = match LibraryInfoRequest::decode(payload) {
            Ok(request) => request,
            Err(err) => return Reply::refuse(Status::BadRequest, format!("LibraryInfoRequest: {err}")),
        };
        let compiler_version = request.compiler_version.as_deref();
        if compiler_version.is_some_and(|version| version.len() > MAX_COMPILER_VERSION_BYTES) {
            return Reply::refuse(Status::BadRequest, "compiler_version is too long");
        }
        tracing::info!(
            peer = %self.peer,
            bitness = request.bitness,
            compiler_type = request.compiler_type,
            compiler_version,
            "Galaxy SDK library info served"
        );
        Reply::ok(&LibraryInfoResponse {
            location: Some(LIBRARY_LOCATION.to_owned()),
            update_status: Some(UpdateStatus::UpdateComplete.into()),
        })
    }

    /// Nothing is ever pushed on a topic here, so a subscription records nothing. It is answered
    /// rather than refused in case the SDK waits on its listeners' subscriptions (plan 2.2).
    fn subscribe_topic(&self, payload: &[u8]) -> Reply {
        let (client_id, _game) = match self.signed_in() {
            Ok(signed_in) => signed_in,
            Err(reply) => return reply,
        };
        let request = match webbroker::SubscribeTopicRequest::decode(payload) {
            Ok(request) => request,
            Err(err) => return Reply::refuse(Status::BadRequest, format!("SubscribeTopicRequest: {err}")),
        };
        let Some(topic) = request.topic else {
            return Reply::refuse(Status::BadRequest, "topic is required");
        };
        if topic.len() > MAX_TOPIC_BYTES {
            return Reply::refuse(Status::BadRequest, "topic is too long");
        }
        tracing::info!(peer = %self.peer, %client_id, topic = topic.as_str(), "Galaxy topic subscription acknowledged");
        Reply::ok(&webbroker::SubscribeTopicResponse { topic: Some(topic) })
    }

    fn auth_info(&mut self, payload: &[u8]) -> Reply {
        let request = match AuthInfoRequest::decode(payload) {
            Ok(request) => request,
            Err(err) => return Reply::refuse(Status::BadRequest, format!("AuthInfoRequest: {err}")),
        };
        let Some(client_id) = request.client_id.as_deref().and_then(ClientId::parse) else {
            if let Some(total) = self.refusals.record(Refusal::MalformedClientId) {
                let length = request.client_id.as_ref().map(String::len);
                tracing::warn!(peer = %self.peer, ?length, total, "Galaxy sign-in refused: malformed client id");
            }
            return Reply::refuse(Status::BadRequest, "client_id must be 1 to 20 decimal digits");
        };
        if request
            .client_secret
            .as_ref()
            .is_some_and(|secret| secret.len() > MAX_SECRET_BYTES)
            || request
                .steam_ticket
                .as_ref()
                .is_some_and(|ticket| ticket.len() > MAX_TICKET_BYTES)
        {
            return Reply::refuse(Status::BadRequest, "client_secret or steam_ticket is too long");
        }
        if client_id.as_str() == GALAXY_CLIENT_ID {
            if let Some(total) = self.refusals.record(Refusal::GalaxyClientId) {
                tracing::warn!(peer = %self.peer, total, "Galaxy sign-in refused: Galaxy's own client id is never a game");
            }
            return Reply::refuse(Status::Forbidden, "this client id is not a game's");
        }
        let Some(game) = self.registry.get(&client_id) else {
            if let Some(total) = self.refusals.record(Refusal::UnregisteredClient) {
                tracing::warn!(peer = %self.peer, %client_id, total, "Galaxy sign-in refused: client id not registered");
            }
            return Reply::refuse(Status::Forbidden, "this game is not registered with decanter's service");
        };
        if self.signed_in.as_ref().is_some_and(|bound| *bound != client_id) {
            return Reply::refuse(Status::Forbidden, "this connection is signed in as another game");
        }
        if self.slot.is_none() {
            let Some(slot) = GameSlot::claim(&game) else {
                if let Some(total) = self.refusals.record(Refusal::GameConnectionsFull) {
                    tracing::warn!(
                        peer = %self.peer,
                        %client_id,
                        total,
                        cap = MAX_CONNECTIONS_PER_GAME,
                        "Galaxy sign-in refused: the game holds its connection cap"
                    );
                }
                return Reply::busy(Duration::from_secs(1), "this game has too many connections open");
            };
            self.slot = Some(slot);
        }
        tracing::info!(peer = %self.peer, %client_id, game_pid = request.game_pid, "Galaxy SDK signed in");
        self.signed_in = Some(client_id);
        // An empty refresh token keeps the SDK from reaching GOG on its own (gap G2); whether every
        // SDK build accepts it is what the first real-game run checks (plan 1.6, section 7).
        Reply::ok(&AuthInfoResponse {
            refresh_token: Some(String::new()),
            environment_type: Some(EnvironmentType::EnvironmentProduction.into()),
            user_id: Some(game.user_id),
            user_name: Some(game.user_name.clone()),
            region: Some(Region::WorldWide.into()),
        })
    }

    fn signed_in(&self) -> Result<(ClientId, Arc<Registered>), Reply> {
        let client_id = self
            .signed_in
            .clone()
            .ok_or_else(|| Reply::refuse(Status::Unauthorized, "sign in with AUTH_INFO first"))?;
        let Some(game) = self.registry.get(&client_id) else {
            tracing::warn!(peer = %self.peer, %client_id, "Galaxy request refused: the game was released");
            return Err(Reply::refuse(
                Status::Forbidden,
                "this game's session with decanter has ended",
            ));
        };
        Ok((client_id, game))
    }

    async fn write_budget(&self, game: &Registered, client_id: &ClientId) -> Result<(), Reply> {
        game.write_budget().await.map_err(|wait| {
            if let Some(total) = self.refusals.record(Refusal::WritesRateLimited) {
                tracing::warn!(peer = %self.peer, %client_id, total, "Galaxy write refused: the game is over its write rate");
            }
            Reply::busy(wait, "too many writes for this game")
        })
    }

    /// comet answers any user id with the signed-in user's data (gap, defects). The SDK echoes the
    /// tagged id it was given; the untagged form names the same account.
    fn is_other_user(game: &Registered, user_id: Option<u64>) -> bool {
        user_id.is_some_and(|asked| asked != game.user_id && asked != game.account_id)
    }

    /// Runs one store call on the blocking pool and turns its failure into the answer the SDK gets,
    /// logged with what was being done.
    async fn with_store<T, F>(
        &self,
        game: &Registered,
        client_id: &ClientId,
        what: &'static str,
        call: F,
    ) -> Result<T, Reply>
    where
        T: Send + 'static,
        F: FnOnce(&dyn GameStore) -> Result<T, SourceError> + Send + 'static,
    {
        let store = Arc::clone(&game.store);
        match spawn_blocking(move || call(&*store)).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(SourceError::Unavailable(reason))) => {
                tracing::warn!(peer = %self.peer, %client_id, what, %reason, "Galaxy store unavailable");
                Err(Reply::refuse(Status::Unavailable, reason))
            }
            Ok(Err(SourceError::NotFound(reason))) => {
                tracing::warn!(peer = %self.peer, %client_id, what, %reason, "Galaxy request named nothing stored");
                Err(Reply::refuse(Status::NotFound, reason))
            }
            Ok(Err(err)) => {
                tracing::warn!(peer = %self.peer, %client_id, what, %err, "Galaxy store failed");
                Err(Reply::refuse(Status::InternalError, format!("{what} failed")))
            }
            Err(err) => {
                tracing::warn!(peer = %self.peer, %client_id, what, %err, "Galaxy store call did not finish");
                Err(Reply::refuse(Status::InternalError, format!("{what} failed")))
            }
        }
    }

    async fn user_achievements(&self, payload: &[u8]) -> Reply {
        let (client_id, game) = match self.signed_in() {
            Ok(signed_in) => signed_in,
            Err(reply) => return reply,
        };
        let request = match GetUserAchievementsRequest::decode(payload) {
            Ok(request) => request,
            Err(err) => return Reply::refuse(Status::BadRequest, format!("GetUserAchievementsRequest: {err}")),
        };
        if Self::is_other_user(&game, request.user_id) {
            return Reply::refuse(Status::Forbidden, "only the signed-in user's achievements are served");
        }
        let lookup = client_id.clone();
        match self
            .with_store(&game, &client_id, "reading achievements", move |store| {
                store.achievements(&lookup)
            })
            .await
        {
            Ok(list) => Reply::ok(&self.to_wire(&client_id, list)),
            Err(reply) => reply,
        }
    }

    async fn unlock_achievement(&self, payload: &[u8]) -> Reply {
        let (client_id, game) = match self.signed_in() {
            Ok(signed_in) => signed_in,
            Err(reply) => return reply,
        };
        let request = match UnlockUserAchievementRequest::decode(payload) {
            Ok(request) => request,
            Err(err) => return Reply::refuse(Status::BadRequest, format!("UnlockUserAchievementRequest: {err}")),
        };
        let Some(achievement_id) = request.achievement_id else {
            return Reply::refuse(Status::BadRequest, "achievement_id is required");
        };
        if let Err(reply) = self.write_budget(&game, &client_id).await {
            return reply;
        }
        let Some(unlocked_at) = unlock_time(request.time) else {
            tracing::warn!(peer = %self.peer, %client_id, "the system clock is unreadable, unlock refused");
            return Reply::refuse(Status::InternalError, "no unlock time could be set");
        };
        let lookup = client_id.clone();
        if let Err(reply) = self
            .with_store(&game, &client_id, "unlocking an achievement", move |store| {
                store.unlock_achievement(&lookup, achievement_id, unlocked_at)
            })
            .await
        {
            return reply;
        }
        tracing::info!(peer = %self.peer, %client_id, achievement_id, unlocked_at, "achievement unlocked locally");
        Reply::done()
    }

    async fn clear_achievement(&self, payload: &[u8]) -> Reply {
        let (client_id, game) = match self.signed_in() {
            Ok(signed_in) => signed_in,
            Err(reply) => return reply,
        };
        let request = match ClearUserAchievementRequest::decode(payload) {
            Ok(request) => request,
            Err(err) => return Reply::refuse(Status::BadRequest, format!("ClearUserAchievementRequest: {err}")),
        };
        let Some(achievement_id) = request.achievement_id else {
            return Reply::refuse(Status::BadRequest, "achievement_id is required");
        };
        if let Err(reply) = self.write_budget(&game, &client_id).await {
            return reply;
        }
        let lookup = client_id.clone();
        if let Err(reply) = self
            .with_store(&game, &client_id, "clearing an achievement", move |store| {
                store.clear_achievement(&lookup, achievement_id)
            })
            .await
        {
            return reply;
        }
        tracing::info!(peer = %self.peer, %client_id, achievement_id, "achievement cleared locally");
        Reply::done()
    }

    async fn stored_stats(&self, game: &Registered, client_id: &ClientId) -> Result<Vec<Stat>, Reply> {
        let lookup = client_id.clone();
        self.with_store(game, client_id, "reading user stats", move |store| {
            store.user_stats(&lookup)
        })
        .await
    }

    async fn user_stats(&self, payload: &[u8]) -> Reply {
        let (client_id, game) = match self.signed_in() {
            Ok(signed_in) => signed_in,
            Err(reply) => return reply,
        };
        let request = match GetUserStatsRequest::decode(payload) {
            Ok(request) => request,
            Err(err) => return Reply::refuse(Status::BadRequest, format!("GetUserStatsRequest: {err}")),
        };
        if Self::is_other_user(&game, request.user_id) {
            return Reply::refuse(Status::Forbidden, "only the signed-in user's stats are served");
        }
        let mut stats = match self.stored_stats(&game, &client_id).await {
            Ok(stats) => stats,
            Err(reply) => return reply,
        };
        if stats.len() > MAX_STATS {
            tracing::warn!(peer = %self.peer, %client_id, total = stats.len(), cap = MAX_STATS, "stat list truncated");
            stats.truncate(MAX_STATS);
        }
        tracing::info!(peer = %self.peer, %client_id, served = stats.len(), "user stats served");
        Reply::ok(&GetUserStatsResponse {
            user_stats: stats.iter().map(stat_to_wire).collect(),
        })
    }

    async fn update_user_stat(&self, payload: &[u8]) -> Reply {
        let (client_id, game) = match self.signed_in() {
            Ok(signed_in) => signed_in,
            Err(reply) => return reply,
        };
        let request = match UpdateUserStatRequest::decode(payload) {
            Ok(request) => request,
            Err(err) => return Reply::refuse(Status::BadRequest, format!("UpdateUserStatRequest: {err}")),
        };
        let Some(stat_id) = request.stat_id else {
            return Reply::refuse(Status::BadRequest, "stat_id is required");
        };
        if let Err(reply) = self.write_budget(&game, &client_id).await {
            return reply;
        }
        let stats = match self.stored_stats(&game, &client_id).await {
            Ok(stats) => stats,
            Err(reply) => return reply,
        };
        let Some(stat) = stats.into_iter().find(|stat| stat.stat_id == stat_id) else {
            tracing::warn!(peer = %self.peer, %client_id, stat_id, "stat update refused: unknown stat id");
            return Reply::refuse(
                Status::NotFound,
                format!("stat {stat_id} is not one of this game's stats"),
            );
        };
        let value = match checked_value(&stat, &request) {
            Ok(value) => value,
            Err(reason) => {
                tracing::warn!(peer = %self.peer, %client_id, stat_key = %stat.key, %reason, "stat update refused");
                return Reply::refuse(Status::BadRequest, reason);
            }
        };
        if value == stat.value {
            return Reply::done();
        }
        let lookup = client_id.clone();
        let key = stat.key.clone();
        if let Err(reply) = self
            .with_store(&game, &client_id, "storing a stat", move |store| {
                store.set_stat(&lookup, &key, value)
            })
            .await
        {
            return reply;
        }
        tracing::info!(peer = %self.peer, %client_id, stat_key = %stat.key, ?value, previous = ?stat.value, "stat stored locally");
        Reply::done()
    }

    async fn time_played(&self, payload: &[u8]) -> Reply {
        let (client_id, game) = match self.signed_in() {
            Ok(signed_in) => signed_in,
            Err(reply) => return reply,
        };
        let request = match GetUserTimePlayedRequest::decode(payload) {
            Ok(request) => request,
            Err(err) => return Reply::refuse(Status::BadRequest, format!("GetUserTimePlayedRequest: {err}")),
        };
        if Self::is_other_user(&game, request.user_id) {
            return Reply::refuse(Status::Forbidden, "only the signed-in user's time played is served");
        }
        let lookup = client_id.clone();
        let seconds = match self
            .with_store(&game, &client_id, "reading time played", move |store| {
                store.seconds_played(&lookup)
            })
            .await
        {
            Ok(seconds) => seconds,
            Err(reply) => return reply,
        };
        let time_played = time_played_units(seconds);
        tracing::info!(peer = %self.peer, %client_id, seconds, time_played, "time played served");
        Reply::ok(&GetUserTimePlayedResponse {
            time_played: Some(time_played),
        })
    }

    fn to_wire(&self, client_id: &ClientId, list: AchievementList) -> GetUserAchievementsResponse {
        let total = list.achievements.len();
        let mut without_id = 0_usize;
        let mut user_achievements: Vec<_> = list
            .achievements
            .into_iter()
            .filter_map(|achievement| {
                let Some(achievement_id) = achievement.achievement_id else {
                    without_id = without_id.saturating_add(1);
                    return None;
                };
                Some(UserAchievement {
                    achievement_id: Some(achievement_id),
                    key: Some(achievement.key),
                    name: Some(achievement.name),
                    description: achievement.description,
                    image_url_locked: achievement.image_url_locked,
                    image_url_unlocked: achievement.image_url_unlocked,
                    visible_while_locked: Some(achievement.visible_while_locked),
                    unlock_time: achievement.unlocked_at.map(NonZeroU32::get),
                    rarity: achievement.rarity,
                    rarity_level_description: achievement.rarity_level_description,
                    rarity_level_slug: achievement.rarity_level_slug,
                })
            })
            .collect();
        if without_id > 0 {
            tracing::warn!(peer = %self.peer, %client_id, without_id, "achievements without a GOG id left out");
        }
        if user_achievements.len() > MAX_ACHIEVEMENTS {
            tracing::warn!(peer = %self.peer, %client_id, total, cap = MAX_ACHIEVEMENTS, "achievement list truncated");
            user_achievements.truncate(MAX_ACHIEVEMENTS);
        }
        tracing::info!(peer = %self.peer, %client_id, served = user_achievements.len(), "achievements served");
        GetUserAchievementsResponse {
            user_achievements,
            language: list.language,
            achievements_mode: list.mode,
        }
    }
}

/// Cut at a character boundary, so that the header never outgrows the codec's cap.
fn bounded(mut description: String) -> String {
    if description.len() > MAX_DESCRIPTION_BYTES {
        let end = (0..=MAX_DESCRIPTION_BYTES)
            .rev()
            .find(|&end| description.is_char_boundary(end))
            .unwrap_or(0);
        description.truncate(end);
    }
    description
}

/// The SDK's time, unless it is unset (`0`) or a wrong clock far ahead: then now (plan 2.1, row 25).
fn unlock_time(requested: Option<u32>) -> Option<NonZeroU32> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u32::try_from(elapsed.as_secs()).ok())
        .and_then(NonZeroU32::new);
    match (requested.and_then(NonZeroU32::new), now) {
        (Some(at), Some(current)) if at.get() <= current.get().saturating_add(MAX_CLOCK_AHEAD_SECONDS) => Some(at),
        (_, Some(current)) => Some(current),
        (sent, None) => sent,
    }
}

/// `time_played` is a `uint32`: saturated rather than wrapped.
fn time_played_units(seconds: u64) -> u32 {
    let units = seconds.checked_div(TIME_PLAYED_SECONDS_PER_UNIT).unwrap_or(seconds);
    u32::try_from(units).unwrap_or(u32::MAX)
}

/// The new value of `stat` if the request may set it: same kind, a finite float, within
/// `min`/`max`, not lower when `increment_only`, and at most `max_change` away from the stored
/// value. comet checks none of these (gap, row 17); nothing else does offline.
#[expect(
    clippy::float_arithmetic,
    reason = "the step from the stored value, in f64, where both kinds are exact"
)]
fn checked_value(stat: &Stat, request: &UpdateUserStatRequest) -> Result<StatValue, String> {
    let key = &stat.key;
    let sent = request
        .value_type
        .and_then(|number| ValueType::try_from(number).ok())
        .ok_or_else(|| format!("stat {key}: value_type is missing or unknown"))?;
    let value = match (stat.kind, sent) {
        (StatKind::Int, ValueType::Int) => request
            .int_value
            .map(StatValue::Int)
            .ok_or_else(|| format!("stat {key}: int_value is missing"))?,
        (StatKind::Float, ValueType::Float) | (StatKind::AvgRate, ValueType::Avgrate) => match request.float_value {
            Some(float) if float.is_finite() => StatValue::Float(float),
            Some(_) => return Err(format!("stat {key}: the value is not a finite number")),
            None => return Err(format!("stat {key}: float_value is missing")),
        },
        (kind, other) => {
            return Err(format!(
                "stat {key} is {kind:?}, the request sent {}",
                other.as_str_name()
            ));
        }
    };
    let new = value.as_f64();
    let current = stat.value.as_f64();
    if let Some(min) = stat.min_value
        && new < min.as_f64()
    {
        return Err(format!("stat {key}: {new} is below its minimum {}", min.as_f64()));
    }
    if let Some(max) = stat.max_value
        && new > max.as_f64()
    {
        return Err(format!("stat {key}: {new} is above its maximum {}", max.as_f64()));
    }
    if stat.increment_only && new < current {
        return Err(format!(
            "stat {key} only increases: {new} is below the stored {current}"
        ));
    }
    if let Some(max_change) = stat.max_change
        && (new - current).abs() > max_change.as_f64()
    {
        return Err(format!(
            "stat {key}: {new} is more than {} away from the stored {current}",
            max_change.as_f64()
        ));
    }
    Ok(value)
}

fn stat_to_wire(stat: &Stat) -> UserStat {
    let int = |value: Option<StatValue>| match value {
        Some(StatValue::Int(int)) => Some(int),
        Some(StatValue::Float(_)) | None => None,
    };
    let float = |value: Option<StatValue>| match value {
        Some(StatValue::Float(float)) => Some(float),
        Some(StatValue::Int(_)) | None => None,
    };
    let value_type = match stat.kind {
        StatKind::Int => ValueType::Int,
        StatKind::Float => ValueType::Float,
        StatKind::AvgRate => ValueType::Avgrate,
    };
    UserStat {
        stat_id: Some(stat.stat_id),
        key: Some(stat.key.clone()),
        value_type: Some(value_type.into()),
        float_value: float(Some(stat.value)),
        int_value: int(Some(stat.value)),
        window_size: stat.window,
        float_default_value: float(stat.default_value),
        int_default_value: int(stat.default_value),
        float_min_value: float(stat.min_value),
        int_min_value: int(stat.min_value),
        float_max_value: float(stat.max_value),
        int_max_value: int(stat.max_value),
        float_max_change: float(stat.max_change),
        int_max_change: int(stat.max_change),
        increment_only: Some(stat.increment_only),
    }
}

fn sort_one(sort: u32, kind: u32) -> Option<MessageType> {
    if sort != COMMUNICATION_SORT {
        return None;
    }
    let value = i32::try_from(kind).ok()?;
    MessageType::try_from(value).ok()
}

fn webbroker_type(sort: u32, kind: u32) -> Option<webbroker::MessageType> {
    if sort != WEBBROKER_SORT {
        return None;
    }
    let value = i32::try_from(kind).ok()?;
    webbroker::MessageType::try_from(value).ok()
}

/// The type an answer carries: the request's own response type where the protocol has one, else
/// the request's type, so even a refusal of something unknown still names what it answers.
fn response_type(sort: u32, kind: u32, message: Option<MessageType>) -> u32 {
    let paired = message.map_or_else(
        || webbroker_type(sort, kind).and_then(webbroker_response).map(i32::from),
        |request| communication_response(request).map(i32::from),
    );
    paired.and_then(|value| u32::try_from(value).ok()).unwrap_or(kind)
}

const fn communication_response(request: MessageType) -> Option<MessageType> {
    Some(match request {
        MessageType::LibraryInfoRequest => MessageType::LibraryInfoResponse,
        MessageType::AuthInfoRequest => MessageType::AuthInfoResponse,
        MessageType::GetUserStatsRequest => MessageType::GetUserStatsResponse,
        MessageType::UpdateUserStatRequest => MessageType::UpdateUserStatResponse,
        MessageType::DeleteUserStatsRequest => MessageType::DeleteUserStatsResponse,
        MessageType::GetGlobalStatsRequest => MessageType::GetGlobalStatsResponse,
        MessageType::GetUserAchievementsRequest => MessageType::GetUserAchievementsResponse,
        MessageType::UnlockUserAchievementRequest => MessageType::UnlockUserAchievementResponse,
        MessageType::ClearUserAchievementRequest => MessageType::ClearUserAchievementResponse,
        MessageType::DeleteUserAchievementsRequest => MessageType::DeleteUserAchievementsResponse,
        MessageType::GetLeaderboardsRequest | MessageType::GetLeaderboardsByKeyRequest => {
            MessageType::GetLeaderboardsResponse
        }
        MessageType::GetLeaderboardEntriesGlobalRequest
        | MessageType::GetLeaderboardEntriesAroundUserRequest
        | MessageType::GetLeaderboardEntriesForUsersRequest => MessageType::GetLeaderboardEntriesResponse,
        MessageType::SetLeaderboardScoreRequest => MessageType::SetLeaderboardScoreResponse,
        MessageType::CreateLeaderboardRequest => MessageType::CreateLeaderboardResponse,
        MessageType::GetUserTimePlayedRequest => MessageType::GetUserTimePlayedResponse,
        MessageType::ShareFileRequest => MessageType::ShareFileResponse,
        MessageType::StartOverlaySessionRequest => MessageType::StartOverlaySessionResponse,
        MessageType::StartStorageSynchronizationRequest => MessageType::StartStorageSynchronizationResponse,
        MessageType::AbortStorageSynchronizationRequest => MessageType::AbortStorageSynchronizationResponse,
        MessageType::ConfigureEnvironmentRequest => MessageType::ConfigureEnvironmentResponse,
        MessageType::UnknownMessage
        | MessageType::LibraryInfoResponse
        | MessageType::AuthInfoResponse
        | MessageType::GetUserStatsResponse
        | MessageType::UpdateUserStatResponse
        | MessageType::DeleteUserStatsResponse
        | MessageType::GetGlobalStatsResponse
        | MessageType::GetUserAchievementsResponse
        | MessageType::UnlockUserAchievementResponse
        | MessageType::ClearUserAchievementResponse
        | MessageType::DeleteUserAchievementsResponse
        | MessageType::GetLeaderboardsResponse
        | MessageType::GetLeaderboardEntriesResponse
        | MessageType::SetLeaderboardScoreResponse
        | MessageType::AuthStateChangeNotification
        | MessageType::CreateLeaderboardResponse
        | MessageType::GetUserTimePlayedResponse
        | MessageType::ShareFileResponse
        | MessageType::StartOverlaySessionResponse
        | MessageType::StartStorageSynchronizationResponse
        | MessageType::StorageSynchronizationProgressNotification
        | MessageType::StorageSynchronizationSuccessNotification
        | MessageType::StorageSynchronizationFailureNotification
        | MessageType::AbortStorageSynchronizationResponse
        | MessageType::OverlayStateChangeNotification
        | MessageType::ConfigureEnvironmentResponse => return None,
    })
}

const fn webbroker_response(request: webbroker::MessageType) -> Option<webbroker::MessageType> {
    match request {
        webbroker::MessageType::AuthRequest => Some(webbroker::MessageType::AuthResponse),
        webbroker::MessageType::SubscribeTopicRequest => Some(webbroker::MessageType::SubscribeTopicResponse),
        webbroker::MessageType::UnknownMessage
        | webbroker::MessageType::AuthResponse
        | webbroker::MessageType::SubscribeTopicResponse
        | webbroker::MessageType::MessageFromTopic => None,
    }
}
