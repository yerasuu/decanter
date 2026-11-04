// SPDX-License-Identifier: GPL-3.0-or-later
//! Throwaway probe of the local Galaxy service over decanter's real database (plan stage S2):
//! creates a scratch database in a temporary directory with `Db::open`, seeds one game's
//! achievements, stats and playtime, serves it through `GalaxyStore`, sends what a game's SDK
//! would, prints the rows afterwards, checks that a GOG refresh keeps the local progress, and
//! deletes the directory. No game and no GOG involved.
//!
//! `cargo run -p decanter-gog --example galaxy_db_probe [port]` (default 19977).

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
    clippy::absolute_paths,
    clippy::too_many_lines,
    reason = "a throwaway probe that prints what it got; read top to bottom, not linted like the service"
)]
#![allow(clippy::missing_docs_in_private_items, reason = "a probe")]

use core::error::Error;
use std::env;
use std::path::Path;
use std::sync::Arc;

use decanter_core::{CachedAchievement, Db, StatDefinition, StatKind, StatValue, Storefront};
use decanter_galaxy::proto::communication::{
    AuthInfoRequest, ClearUserAchievementRequest, GetUserAchievementsRequest, GetUserAchievementsResponse,
    GetUserStatsRequest, GetUserStatsResponse, GetUserTimePlayedRequest, GetUserTimePlayedResponse, MessageType,
    UnlockUserAchievementRequest, UpdateUserStatRequest, ValueType,
};
use decanter_galaxy::proto::pb::Header;
use decanter_galaxy::{Account, ClientId, Config, ResponseHeader, Service, read_frame, write_frame};
use decanter_gog::GalaxyStore;
use prost::Message;
use tokio::net::TcpStream;
use tokio::sync::oneshot;

type Probe<T> = Result<T, Box<dyn Error + Send + Sync>>;

const CLIENT_ID: &str = "48766840879051524";
const PRODUCT_ID: &str = "1422357892";
const ACCOUNT_ID: u64 = 46_988_240_917_958_416;
const PORT: u16 = 19_977;
const MAX_RESPONSE_BYTES: u32 = 16 << 20;

struct Client {
    stream: TcpStream,
    oseq: u32,
}

impl Client {
    async fn call(&mut self, kind: MessageType, request: &impl Message) -> Probe<Vec<u8>> {
        self.oseq = self.oseq.saturating_add(1);
        let header = Header {
            sort: 1,
            r#type: u32::try_from(i32::from(kind))?,
            size: None,
            oseq: Some(self.oseq),
        };
        write_frame(&mut self.stream, header, &request.encode_to_vec()).await?;
        let frame = read_frame::<ResponseHeader, _>(&mut self.stream, MAX_RESPONSE_BYTES)
            .await?
            .ok_or("the service closed the connection")?;
        println!(
            "  <- type {} code {:?} description {:?}",
            frame.header.r#type,
            frame.header.code(),
            frame.header.description.as_deref().unwrap_or("")
        );
        Ok(frame.payload)
    }
}

fn achievement(id: i64, key: &str, unlocked_at: Option<&str>) -> CachedAchievement {
    CachedAchievement {
        achievement_key: key.to_owned(),
        name: key.to_owned(),
        description: None,
        icon_unlocked_url: None,
        icon_locked_url: None,
        unlocked: unlocked_at.is_some(),
        unlocked_at: unlocked_at.map(str::to_owned),
        achievement_id: Some(id),
        visible_while_locked: Some(true),
        rarity: Some(12.5),
        rarity_level_description: None,
        rarity_level_slug: None,
        mode: Some("all_visible".to_owned()),
    }
}

fn stats(kills: i32, best_time: f64) -> Vec<(StatDefinition, StatValue)> {
    let mut kills_definition = StatDefinition::new("KILLS".to_owned(), "7001".to_owned(), StatKind::Int);
    kills_definition.increment_only = true;
    kills_definition.min_value = Some(StatValue::Int(0));
    kills_definition.max_value = Some(StatValue::Int(1_000));
    kills_definition.max_change = Some(StatValue::Int(50));
    let mut time_definition = StatDefinition::new("BEST_TIME".to_owned(), "7002".to_owned(), StatKind::Float);
    time_definition.min_value = Some(StatValue::Float(0.0));
    vec![
        (kills_definition, StatValue::Int(kills)),
        (time_definition, StatValue::Float(best_time)),
    ]
}

/// GOG's answer before the session: 5501 unlocked, 5502 locked, `KILLS` 10, `BEST_TIME` 90.5.
fn seed(path: &Path) -> Probe<()> {
    let mut db = Db::open(path)?;
    let account = ACCOUNT_ID.to_string();
    db.upsert_achievements(
        &account,
        Storefront::Gog,
        PRODUCT_ID,
        &[
            achievement(5_501, "FIRST_BOSS", Some("2025-11-03T20:00:00+0000")),
            achievement(5_502, "ALL_CHARACTERS", None),
        ],
    )?;
    db.refresh_user_stats(&account, Storefront::Gog, PRODUCT_ID, &stats(10, 90.5))?;
    db.add_playtime(&account, Storefront::Gog, PRODUCT_ID, 3_600)?;
    Ok(())
}

async fn session(port: u16) -> Probe<()> {
    let user_id = (2_u64 << 56_u32) | ACCOUNT_ID;
    let mut client = Client {
        stream: TcpStream::connect(("127.0.0.1", port)).await?,
        oseq: 0,
    };
    println!("-> AUTH_INFO for client {CLIENT_ID}");
    client
        .call(
            MessageType::AuthInfoRequest,
            &AuthInfoRequest {
                client_id: Some(CLIENT_ID.to_owned()),
                ..AuthInfoRequest::default()
            },
        )
        .await?;
    for (label, id, time) in [
        ("5502 at 1762300000", 5_502, Some(1_762_300_000)),
        (
            "5502 again at 1762399999 (keeps the first time)",
            5_502,
            Some(1_762_399_999),
        ),
        ("9999, which the game does not have", 9_999, None),
    ] {
        println!("-> UNLOCK_USER_ACHIEVEMENT {label}");
        client
            .call(
                MessageType::UnlockUserAchievementRequest,
                &UnlockUserAchievementRequest {
                    achievement_id: Some(id),
                    time,
                },
            )
            .await?;
    }
    println!("-> CLEAR_USER_ACHIEVEMENT 5501");
    client
        .call(
            MessageType::ClearUserAchievementRequest,
            &ClearUserAchievementRequest {
                achievement_id: Some(5_501),
            },
        )
        .await?;
    println!("-> GET_USER_ACHIEVEMENTS");
    let payload = client
        .call(
            MessageType::GetUserAchievementsRequest,
            &GetUserAchievementsRequest { user_id: Some(user_id) },
        )
        .await?;
    for achievement in GetUserAchievementsResponse::decode(payload.as_slice())?.user_achievements {
        println!(
            "  id {} key {} unlock_time {}",
            achievement.achievement_id(),
            achievement.key(),
            achievement.unlock_time()
        );
    }
    let int_update = |value: i32| UpdateUserStatRequest {
        stat_id: Some(7_001),
        value_type: Some(ValueType::Int.into()),
        float_value: None,
        int_value: Some(value),
    };
    for (label, request) in [
        ("KILLS 10 -> 40", int_update(40)),
        ("KILLS 40 -> 200 (more than max_change 50)", int_update(200)),
        ("KILLS 40 -> 1001 (above max 1000)", int_update(1_001)),
        (
            "BEST_TIME -> 75.25",
            UpdateUserStatRequest {
                stat_id: Some(7_002),
                value_type: Some(ValueType::Float.into()),
                float_value: Some(75.25),
                int_value: None,
            },
        ),
    ] {
        println!("-> UPDATE_USER_STAT {label}");
        client.call(MessageType::UpdateUserStatRequest, &request).await?;
    }
    println!("-> GET_USER_STATS");
    let payload = client
        .call(
            MessageType::GetUserStatsRequest,
            &GetUserStatsRequest { user_id: Some(user_id) },
        )
        .await?;
    for stat in GetUserStatsResponse::decode(payload.as_slice())?.user_stats {
        println!(
            "  id {} key {} int {:?} float {:?}",
            stat.stat_id(),
            stat.key(),
            stat.int_value,
            stat.float_value
        );
    }
    println!("-> GET_USER_TIME_PLAYED");
    let payload = client
        .call(
            MessageType::GetUserTimePlayedRequest,
            &GetUserTimePlayedRequest { user_id: Some(user_id) },
        )
        .await?;
    println!(
        "  time_played {} (seconds: 3600 stored plus this session; unit UNVERIFIED)",
        GetUserTimePlayedResponse::decode(payload.as_slice())?.time_played()
    );
    Ok(())
}

fn print_rows(path: &Path, when: &str) -> Probe<()> {
    let conn = rusqlite::Connection::open(path)?;
    println!("rows {when}:");
    let mut achievements = conn.prepare(
        "SELECT achievement_key, unlocked, unlocked_at, changed, changed_at IS NOT NULL FROM achievement_cache
         ORDER BY achievement_key",
    )?;
    let rows = achievements.query_map([], |row| {
        Ok(format!(
            "  achievement_cache {} unlocked={} unlocked_at={:?} changed={} changed_at set={}",
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, bool>(4)?
        ))
    })?;
    for row in rows {
        println!("{}", row?);
    }
    let mut stats = conn.prepare("SELECT stat_key, value, changed FROM user_stats ORDER BY stat_key")?;
    let rows = stats.query_map([], |row| {
        Ok(format!(
            "  user_stats {} value={:?} changed={}",
            row.get::<_, String>(0)?,
            row.get::<_, rusqlite::types::Value>(1)?,
            row.get::<_, i64>(2)?
        ))
    })?;
    for row in rows {
        println!("{}", row?);
    }
    Ok(())
}

/// The S2 check that a GOG answer still saying "locked / old value" keeps the local progress.
fn refresh_from_gog(path: &Path) -> Probe<()> {
    let mut db = Db::open(path)?;
    let account = ACCOUNT_ID.to_string();
    db.upsert_achievements(
        &account,
        Storefront::Gog,
        PRODUCT_ID,
        &[
            achievement(5_501, "FIRST_BOSS", Some("2025-11-03T20:00:00+0000")),
            achievement(5_502, "ALL_CHARACTERS", None),
        ],
    )?;
    let kept = db.refresh_user_stats(&account, Storefront::Gog, PRODUCT_ID, &stats(10, 90.5))?;
    println!("GOG refresh with the pre-session answer: {kept} stats kept their local value");
    Ok(())
}

async fn run(path: &Path, port: u16) -> Probe<()> {
    seed(path)?;
    print_rows(path, "after seeding")?;
    let client_id = ClientId::parse(CLIENT_ID).ok_or("the probe's client id is not valid")?;
    let store = GalaxyStore::new(
        path.to_owned(),
        ACCOUNT_ID.to_string(),
        client_id.clone(),
        PRODUCT_ID.to_owned(),
    );
    let config = Config {
        port,
        client_id,
        account: Account {
            user_id: ACCOUNT_ID,
            user_name: "probe".to_owned(),
        },
    };
    let service = Service::bind(config, Arc::new(store)).await?;
    println!("service on {}", service.local_addr());
    let (stop, stopped) = oneshot::channel::<()>();
    let running = tokio::spawn(service.run(async move {
        let _closed = stopped.await;
    }));
    let result = session(port).await;
    let _ignored = stop.send(());
    running.await?;
    result?;
    print_rows(path, "after the session")?;
    refresh_from_gog(path)?;
    print_rows(path, "after the GOG refresh")
}

#[tokio::main]
async fn main() -> Probe<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    let port = match env::args().nth(1) {
        Some(text) => text.parse()?,
        None => PORT,
    };
    let dir = env::temp_dir().join(format!("decanter-galaxy-db-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let result = run(&dir.join("decanter.db"), port).await;
    std::fs::remove_dir_all(&dir)?;
    println!("removed {}", dir.display());
    result
}
