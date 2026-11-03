// SPDX-License-Identifier: GPL-3.0-or-later
//! One connection's requests (plan section 2). Every request gets an answer, `NOT_IMPLEMENTED`
//! included, unlike comet, which leaves the SDK waiting on its own timeout (gap 1.1).

use core::net::SocketAddr;
use core::num::NonZeroU32;
use std::collections::BTreeMap;
use std::sync::Arc;

use prost::Message as _;
use tokio::task::spawn_blocking;

use crate::codec::Frame;
use crate::header::ResponseHeader;
use crate::proto::communication::{
    AuthInfoRequest, AuthInfoResponse, EnvironmentType, GetUserAchievementsRequest, GetUserAchievementsResponse,
    MessageType, Region, get_user_achievements_response::UserAchievement,
};
use crate::proto::pb::Header;
use crate::proto::pb::response::Status;
use crate::proto::webbroker;
use crate::service::Shared;
use crate::source::{AchievementList, ClientId, SourceError};

const COMMUNICATION_SORT: u32 = 1;
const WEBBROKER_SORT: u32 = 2;
/// decanter's own login client (`crates/gog/src/api/client/auth.rs`): never a game, so it never
/// signs in here (plan 1.5).
const GALAXY_CLIENT_ID: &str = "46899977096215655";
/// GOG's client secrets are 64 hex digits; the cap leaves room without letting one fill a frame.
const MAX_SECRET_BYTES: usize = 128;
/// A Steam session ticket in hex is about two kilobytes. It is never read, only bounded.
const MAX_TICKET_BYTES: usize = 4096;
/// Plan 2.4; GOG games have far fewer.
const MAX_ACHIEVEMENTS: usize = 4096;

struct Reply {
    code: Status,
    description: Option<String>,
    payload: Vec<u8>,
}

impl Reply {
    fn ok(message: &impl prost::Message) -> Self {
        Self {
            code: Status::Ok,
            description: None,
            payload: message.encode_to_vec(),
        }
    }

    fn refuse(code: Status, description: impl Into<String>) -> Self {
        Self {
            code,
            description: Some(description.into()),
            payload: Vec::new(),
        }
    }
}

pub(crate) struct Connection {
    peer: SocketAddr,
    shared: Arc<Shared>,
    /// Bound by the first accepted `AUTH_INFO` and never rebound (comet does the same, gap 2).
    signed_in: Option<ClientId>,
    next_oseq: u32,
    /// `(sort, type, status code)` to how many times, logged when the connection ends.
    counts: BTreeMap<(u32, u32, i32), u32>,
}

impl Connection {
    pub(crate) const fn new(peer: SocketAddr, shared: Arc<Shared>) -> Self {
        Self {
            peer,
            shared,
            signed_in: None,
            next_oseq: 1,
            counts: BTreeMap::new(),
        }
    }

    pub(crate) const fn counts(&self) -> &BTreeMap<(u32, u32, i32), u32> {
        &self.counts
    }

    pub(crate) async fn handle(&mut self, frame: &Frame<Header>) -> (ResponseHeader, Vec<u8>) {
        let Header { sort, r#type, oseq, .. } = frame.header;
        let message = sort_one(sort, r#type);
        let reply = match message {
            Some(MessageType::AuthInfoRequest) => self.auth_info(&frame.payload),
            Some(MessageType::GetUserAchievementsRequest) => self.user_achievements(&frame.payload).await,
            _ => Reply::refuse(
                Status::NotImplemented,
                "not implemented by decanter's local Galaxy service",
            ),
        };
        let code = i32::from(reply.code);
        let count = self.counts.entry((sort, r#type, code)).or_default();
        *count = count.saturating_add(1);
        if reply.code == Status::NotImplemented && *count == 1 {
            let name = message.map_or_else(
                || webbroker_type(sort, r#type).map_or("unknown", |known| known.as_str_name()),
                |known| known.as_str_name(),
            );
            tracing::warn!(peer = %self.peer, sort, r#type, name, "Galaxy request not implemented, answered 501");
        }
        let header = ResponseHeader {
            sort,
            r#type: response_type(sort, r#type, message),
            size: None,
            oseq: Some(self.next_oseq),
            rseq: oseq,
            code: Some(code),
            description: reply.description,
            retry_after: None,
        };
        self.next_oseq = self.next_oseq.wrapping_add(1);
        (header, reply.payload)
    }

    fn auth_info(&mut self, payload: &[u8]) -> Reply {
        let request = match AuthInfoRequest::decode(payload) {
            Ok(request) => request,
            Err(err) => return Reply::refuse(Status::BadRequest, format!("AuthInfoRequest: {err}")),
        };
        let Some(client_id) = request.client_id.as_deref().and_then(ClientId::parse) else {
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
        if client_id.as_str() == GALAXY_CLIENT_ID || client_id != self.shared.client_id {
            tracing::warn!(peer = %self.peer, %client_id, "Galaxy sign-in refused: client id not registered");
            return Reply::refuse(Status::Forbidden, "this game is not registered with decanter's service");
        }
        if self.signed_in.as_ref().is_some_and(|bound| *bound != client_id) {
            return Reply::refuse(Status::Forbidden, "this connection is signed in as another game");
        }
        tracing::info!(peer = %self.peer, %client_id, game_pid = request.game_pid, "Galaxy SDK signed in");
        self.signed_in = Some(client_id);
        // An empty refresh token keeps the SDK from reaching GOG on its own (gap G2); whether every
        // SDK build accepts it is what the first real-game run checks (plan 1.6, section 7).
        Reply::ok(&AuthInfoResponse {
            refresh_token: Some(String::new()),
            environment_type: Some(EnvironmentType::EnvironmentProduction.into()),
            user_id: Some(self.shared.user_id),
            user_name: Some(self.shared.user_name.clone()),
            region: Some(Region::WorldWide.into()),
        })
    }

    async fn user_achievements(&self, payload: &[u8]) -> Reply {
        let Some(client_id) = self.signed_in.clone() else {
            return Reply::refuse(Status::Unauthorized, "sign in with AUTH_INFO first");
        };
        let request = match GetUserAchievementsRequest::decode(payload) {
            Ok(request) => request,
            Err(err) => return Reply::refuse(Status::BadRequest, format!("GetUserAchievementsRequest: {err}")),
        };
        // comet answers any user id with the signed-in user's list (gap, defects). The SDK echoes
        // the tagged id it was given; the untagged form names the same account.
        if request
            .user_id
            .is_some_and(|user_id| user_id != self.shared.user_id && user_id != self.shared.account_id)
        {
            return Reply::refuse(Status::Forbidden, "only the signed-in user's achievements are served");
        }
        let source = Arc::clone(&self.shared.source);
        let lookup = client_id.clone();
        let list = match spawn_blocking(move || source.achievements(&lookup)).await {
            Ok(Ok(list)) => list,
            Ok(Err(SourceError::Unavailable(reason))) => {
                tracing::warn!(peer = %self.peer, %client_id, %reason, "achievements unavailable to the SDK");
                return Reply::refuse(Status::Unavailable, reason);
            }
            Ok(Err(err)) => {
                tracing::warn!(peer = %self.peer, %client_id, %err, "reading achievements for the SDK failed");
                return Reply::refuse(Status::InternalError, "achievements could not be read");
            }
            Err(err) => {
                tracing::warn!(peer = %self.peer, %client_id, %err, "the achievement read did not finish");
                return Reply::refuse(Status::InternalError, "achievements could not be read");
            }
        };
        Reply::ok(&self.to_wire(&client_id, list))
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
