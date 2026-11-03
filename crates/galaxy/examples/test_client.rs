// SPDX-License-Identifier: GPL-3.0-or-later
//! Throwaway test client for the local Galaxy service (plan stage S1): starts the service over an
//! in-memory achievement list, then signs in and reads it back through the real codec, the way a
//! game's SDK would. Not part of the app; no game and no GOG involved.
//!
//! `cargo run -p decanter-galaxy --example test_client [port]` (default 9977).

#![expect(
    clippy::print_stdout,
    reason = "this example's whole job is to narrate what it's doing"
)]
#![expect(
    clippy::implicit_return,
    reason = "needless_return elsewhere already wants no explicit return"
)]
#![expect(
    clippy::question_mark_used,
    reason = "a probe stops at its first failure and says which step it was"
)]
#![expect(clippy::single_call_fn, reason = "each step of the probe is split out to name it")]
#![expect(clippy::large_stack_frames, reason = "tokio futures in a probe; not worth boxing")]
#![expect(
    clippy::std_instead_of_alloc,
    reason = "a desktop probe on std, with no no_std target to justify alloc::"
)]
#![expect(
    clippy::separated_literal_suffix,
    reason = "`0_u8` is the workspace's spelling; unseparated_literal_suffix asks the opposite"
)]
#![expect(
    clippy::shadow_unrelated,
    clippy::use_debug,
    clippy::default_numeric_fallback,
    clippy::arbitrary_source_item_ordering,
    clippy::big_endian_bytes,
    clippy::absolute_paths,
    reason = "a throwaway probe that prints what it got; read top to bottom, not linted like the service"
)]
#![allow(clippy::missing_docs_in_private_items, reason = "a probe")]

use core::error::Error;
use core::num::NonZeroU32;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;

use decanter_galaxy::proto::communication::{
    AuthInfoRequest, AuthInfoResponse, GetUserAchievementsRequest, GetUserAchievementsResponse, LibraryInfoRequest,
    MessageType,
};
use decanter_galaxy::proto::pb::Header;
use decanter_galaxy::proto::webbroker::{self, SubscribeTopicRequest};
use decanter_galaxy::{
    Account, Achievement, AchievementList, AchievementSource, ClientId, Config, DEFAULT_PORT, ResponseHeader, Service,
    SourceError, read_frame, write_frame,
};
use prost::Message;
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpStream;
use tokio::sync::oneshot;

type Probe<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// Ys Origin's client id, the plan's first test title (section 7); any registered id would do.
const CLIENT_ID: &str = "48766840879051524";
const ACCOUNT_ID: u64 = 46_988_240_917_958_416;
/// Responses carry whole lists, so the client reads more than the service accepts per request.
const MAX_RESPONSE_BYTES: u32 = 16 << 20;

struct MemorySource {
    games: HashMap<ClientId, AchievementList>,
}

impl AchievementSource for MemorySource {
    fn achievements(&self, client_id: &ClientId) -> Result<AchievementList, SourceError> {
        self.games
            .get(client_id)
            .cloned()
            .ok_or_else(|| SourceError::Unavailable(format!("no achievement list was fetched for {client_id}")))
    }
}

fn seeded(client_id: &ClientId) -> MemorySource {
    let achievement = |id: Option<u64>, key: &str, name: &str, unlocked_at: Option<u32>| Achievement {
        achievement_id: id,
        key: key.to_owned(),
        name: name.to_owned(),
        description: Some(format!("{name}, seeded by the test client")),
        image_url_locked: Some(format!("https://images.gog.com/{key}_locked.png")),
        image_url_unlocked: Some(format!("https://images.gog.com/{key}.png")),
        visible_while_locked: true,
        unlocked_at: unlocked_at.and_then(NonZeroU32::new),
        rarity: Some(12.5),
        rarity_level_description: Some("Rare".to_owned()),
        rarity_level_slug: Some("rare".to_owned()),
    };
    let list = AchievementList {
        achievements: vec![
            achievement(Some(5_501), "FIRST_BOSS", "First boss", Some(1_762_200_000)),
            achievement(Some(5_502), "ALL_CHARACTERS", "All characters", None),
            achievement(None, "NO_GOG_ID", "Never fetched with an id", None),
        ],
        language: Some("en-US".to_owned()),
        mode: Some("all_visible".to_owned()),
    };
    MemorySource {
        games: HashMap::from([(client_id.clone(), list)]),
    }
}

struct Client {
    stream: TcpStream,
    oseq: u32,
}

impl Client {
    async fn connect(port: u16) -> Probe<Self> {
        Ok(Self {
            stream: TcpStream::connect(("127.0.0.1", port)).await?,
            oseq: 0,
        })
    }

    async fn call(&mut self, sort: u32, kind: i32, request: &impl Message) -> Probe<(ResponseHeader, Vec<u8>)> {
        self.oseq = self.oseq.saturating_add(1);
        let header = Header {
            sort,
            r#type: u32::try_from(kind)?,
            size: None,
            oseq: Some(self.oseq),
        };
        write_frame(&mut self.stream, header, &request.encode_to_vec()).await?;
        let frame = read_frame::<ResponseHeader, _>(&mut self.stream, MAX_RESPONSE_BYTES)
            .await?
            .ok_or("the service closed the connection")?;
        let header = frame.header;
        println!(
            "  <- sort {} type {} rseq {:?} code {:?} description {:?} payload {} bytes",
            header.sort,
            header.r#type,
            header.rseq,
            header.code(),
            header.description.as_deref().unwrap_or(""),
            frame.payload.len()
        );
        Ok((header, frame.payload))
    }
}

/// The plan's S1 check that the hand-written header and the generated one agree on the wire.
fn header_round_trip() -> Probe<()> {
    let response = ResponseHeader {
        sort: 1,
        r#type: 4,
        size: Some(9),
        oseq: Some(7),
        rseq: Some(3),
        code: Some(501),
        description: Some("not implemented".to_owned()),
        retry_after: Some(2),
    };
    let bytes = response.encode_to_vec();
    let generated = Header::decode(bytes.as_slice())?;
    let back = ResponseHeader::decode(bytes.as_slice())?;
    let fields_agree = (generated.sort, generated.r#type, generated.size, generated.oseq) == (1, 4, Some(9), Some(7));
    println!(
        "header round trip: generated Header reads {generated:?}; ResponseHeader decodes back equal: {}; \
         fields 1-4 agree: {fields_agree}",
        back == response
    );
    if back != response || !fields_agree {
        return Err("ResponseHeader and the generated Header disagree".into());
    }
    Ok(())
}

async fn signed_in_session(port: u16, user_id: u64) -> Probe<()> {
    let mut client = Client::connect(port).await?;
    let sort = 1;

    println!("-> GET_USER_ACHIEVEMENTS before sign-in");
    client
        .call(
            sort,
            MessageType::GetUserAchievementsRequest.into(),
            &GetUserAchievementsRequest { user_id: None },
        )
        .await?;

    println!("-> LIBRARY_INFO (not implemented in S1)");
    client
        .call(
            sort,
            MessageType::LibraryInfoRequest.into(),
            &LibraryInfoRequest {
                bitness: Some(1),
                compiler_type: Some(3),
                compiler_version: Some("19".to_owned()),
            },
        )
        .await?;

    println!("-> AUTH_INFO for client {CLIENT_ID}");
    let (_, payload) = client
        .call(
            sort,
            MessageType::AuthInfoRequest.into(),
            &AuthInfoRequest {
                client_id: Some(CLIENT_ID.to_owned()),
                client_secret: Some("0".repeat(64)),
                platform_type: Some(1),
                steam_ticket: None,
                game_pid: Some(4242),
                openid: None,
            },
        )
        .await?;
    let auth = AuthInfoResponse::decode(payload.as_slice())?;
    println!(
        "  signed in: user_id {:#x} (tag {}), user_name {:?}, refresh_token {:?}, environment {:?}, region {:?}",
        auth.user_id(),
        auth.user_id() >> 56_u32,
        auth.user_name(),
        auth.refresh_token(),
        auth.environment_type(),
        auth.region()
    );

    println!("-> GET_USER_ACHIEVEMENTS for the signed-in user");
    let (_, payload) = client
        .call(
            sort,
            MessageType::GetUserAchievementsRequest.into(),
            &GetUserAchievementsRequest { user_id: Some(user_id) },
        )
        .await?;
    let list = GetUserAchievementsResponse::decode(payload.as_slice())?;
    println!("  language {:?}, mode {:?}", list.language(), list.achievements_mode());
    for achievement in &list.user_achievements {
        println!(
            "  id {} key {} name {:?} unlock_time {} rarity {} ({}) visible_while_locked {}",
            achievement.achievement_id(),
            achievement.key(),
            achievement.name(),
            achievement.unlock_time(),
            achievement.rarity(),
            achievement.rarity_level_slug(),
            achievement.visible_while_locked()
        );
    }

    println!("-> GET_USER_ACHIEVEMENTS for another user");
    client
        .call(
            sort,
            MessageType::GetUserAchievementsRequest.into(),
            &GetUserAchievementsRequest {
                user_id: Some(user_id.wrapping_add(1)),
            },
        )
        .await?;

    println!("-> sort 2 SUBSCRIBE_TOPIC (not implemented in S1)");
    client
        .call(
            2,
            webbroker::MessageType::SubscribeTopicRequest.into(),
            &SubscribeTopicRequest {
                topic: Some("friends".to_owned()),
            },
        )
        .await?;

    println!("-> unknown sort 9 type 77");
    client.call(9, 77, &SubscribeTopicRequest { topic: None }).await?;
    Ok(())
}

async fn refused_sessions(port: u16) -> Probe<()> {
    println!("-> AUTH_INFO for an unregistered client on a new connection");
    let mut client = Client::connect(port).await?;
    client
        .call(
            1,
            MessageType::AuthInfoRequest.into(),
            &AuthInfoRequest {
                client_id: Some("12345".to_owned()),
                ..AuthInfoRequest::default()
            },
        )
        .await?;

    println!("-> AUTH_INFO with a client id that is not a number");
    client
        .call(
            1,
            MessageType::AuthInfoRequest.into(),
            &AuthInfoRequest {
                client_id: Some("../../etc".to_owned()),
                ..AuthInfoRequest::default()
            },
        )
        .await?;

    println!("-> a frame whose header claims a 1 MiB payload");
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    let header = Header {
        sort: 1,
        r#type: 3,
        size: Some(1 << 20),
        oseq: Some(1),
    }
    .encode_to_vec();
    stream.write_all(&u16::try_from(header.len())?.to_be_bytes()).await?;
    stream.write_all(&header).await?;
    let closed = read_frame::<ResponseHeader, _>(&mut stream, MAX_RESPONSE_BYTES)
        .await?
        .is_none();
    println!("  <- connection closed by the service: {closed}");

    println!("-> a zero-length header");
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream.write_all(&[0, 0]).await?;
    let closed = read_frame::<ResponseHeader, _>(&mut stream, MAX_RESPONSE_BYTES)
        .await?
        .is_none();
    println!("  <- connection closed by the service: {closed}");
    Ok(())
}

#[tokio::main]
async fn main() -> Probe<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    let port = match env::args().nth(1) {
        Some(text) => text.parse()?,
        None => DEFAULT_PORT,
    };
    header_round_trip()?;

    let client_id = ClientId::parse(CLIENT_ID).ok_or("the probe's client id is not valid")?;
    let config = Config {
        port,
        client_id: client_id.clone(),
        account: Account {
            user_id: ACCOUNT_ID,
            user_name: "probe".to_owned(),
        },
    };
    let service = Service::bind(config, Arc::new(seeded(&client_id))).await?;
    println!("service on {}", service.local_addr());
    let (stop, stopped) = oneshot::channel::<()>();
    let running = tokio::spawn(service.run(async move {
        let _closed = stopped.await;
    }));

    let tagged = (2_u64 << 56_u32) | ACCOUNT_ID;
    let result = match signed_in_session(port, tagged).await {
        Ok(()) => refused_sessions(port).await,
        Err(err) => Err(err),
    };
    let _ignored = stop.send(());
    running.await?;
    result
}
