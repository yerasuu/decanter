// SPDX-License-Identifier: GPL-3.0-or-later
//! Throwaway probe of the local Galaxy service's lifecycle (plan stage S3) on the real port 9977,
//! over a scratch database and scratch installs in a temporary directory: two games registered one
//! after the other and at once, the same game twice, the port held by another program, the kill
//! switch, and a build without Galaxy. Each case checks the port is let go when the last game
//! leaves. No game, and no GOG: `serve_galaxy` is only driven down the paths that return before
//! it seeds.
//!
//! Run with `cargo run -p decanter-gog --example galaxy_lifecycle_probe`.

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
    clippy::arithmetic_side_effects,
    clippy::shadow_reuse,
    clippy::struct_field_names,
    reason = "a throwaway probe that prints what it got; read top to bottom, not linted like the service"
)]
#![allow(clippy::missing_docs_in_private_items, reason = "a probe")]

use core::error::Error;
use core::time::Duration;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use decanter_core::{CachedAchievement, Db, Storefront};
use decanter_galaxy::proto::communication::{
    AuthInfoRequest, GetUserAchievementsRequest, GetUserAchievementsResponse, MessageType,
};
use decanter_galaxy::proto::pb::Header;
use decanter_galaxy::proto::pb::response::Status;
use decanter_galaxy::{Account, ClientId, DEFAULT_PORT, Host, Lease, ResponseHeader, read_frame, write_frame};
use decanter_gog::{GalaxyStore, serve_galaxy};
use prost::Message;
use tokio::net::TcpStream;
use tokio::time::sleep;

type Probe<T> = Result<T, Box<dyn Error + Send + Sync>>;

const ACCOUNT_ID: u64 = 46_988_240_917_958_416;
const MAX_RESPONSE_BYTES: u32 = 16 << 20;

struct Game {
    product_id: &'static str,
    client_id: &'static str,
    achievement_id: i64,
}

const YS_ORIGIN: Game = Game {
    product_id: "1422357892",
    client_id: "48766840879051524",
    achievement_id: 5_501,
};
const YS_I: Game = Game {
    product_id: "1422440106",
    client_id: "49964625586061620",
    achievement_id: 6_601,
};

struct Client {
    stream: TcpStream,
    oseq: u32,
}

impl Client {
    async fn connect() -> Probe<Self> {
        Ok(Self {
            stream: TcpStream::connect(("127.0.0.1", DEFAULT_PORT)).await?,
            oseq: 0,
        })
    }

    /// The answer's status code and payload.
    async fn call(&mut self, kind: MessageType, request: &impl Message) -> Probe<(Status, Vec<u8>)> {
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
        Ok((frame.header.code(), frame.payload))
    }

    async fn sign_in(&mut self, game: &Game) -> Probe<Status> {
        let request = AuthInfoRequest {
            client_id: Some(game.client_id.to_owned()),
            ..AuthInfoRequest::default()
        };
        Ok(self.call(MessageType::AuthInfoRequest, &request).await?.0)
    }

    /// `code` and how many achievements came back.
    async fn achievements(&mut self) -> Probe<(Status, usize)> {
        let (code, payload) = self
            .call(
                MessageType::GetUserAchievementsRequest,
                &GetUserAchievementsRequest { user_id: None },
            )
            .await?;
        let count = GetUserAchievementsResponse::decode(payload.as_slice())?
            .user_achievements
            .len();
        Ok((code, count))
    }
}

fn seed(db_path: &Path) -> Probe<()> {
    let mut db = Db::open(db_path)?;
    let account = ACCOUNT_ID.to_string();
    db.upsert_account(&account, Storefront::Gog, "default", "probe-user", 0)?;
    for game in [&YS_ORIGIN, &YS_I] {
        db.upsert_achievements(
            &account,
            Storefront::Gog,
            game.product_id,
            &[CachedAchievement {
                achievement_key: "FIRST".to_owned(),
                name: "First".to_owned(),
                description: None,
                icon_unlocked_url: None,
                icon_locked_url: None,
                unlocked: false,
                unlocked_at: None,
                achievement_id: Some(game.achievement_id),
                visible_while_locked: Some(true),
                rarity: None,
                rarity_level_description: None,
                rarity_level_slug: None,
                mode: None,
            }],
        )?;
    }
    Ok(())
}

/// An install root holding only the game's `goggame-<id>.info`, with or without a client id.
fn install(dir: &Path, name: &str, product_id: &str, client_id: Option<&str>) -> Probe<PathBuf> {
    let root = dir.join(name);
    std::fs::create_dir_all(&root)?;
    let client = client_id.map_or_else(String::new, |id| format!("\"clientId\": \"{id}\",\n  "));
    std::fs::write(
        root.join(format!("goggame-{product_id}.info")),
        format!("{{\n  {client}\"gameId\": \"{product_id}\",\n  \"playTasks\": []\n}}\n"),
    )?;
    Ok(root)
}

async fn register(host: &Arc<Host>, db_path: &Path, game: &Game) -> Probe<Lease> {
    let client_id = ClientId::parse(game.client_id).ok_or("the probe's client id is not valid")?;
    let store = GalaxyStore::new(
        db_path.to_owned(),
        ACCOUNT_ID.to_string(),
        client_id.clone(),
        game.product_id.to_owned(),
    );
    let account = Account {
        user_id: ACCOUNT_ID,
        user_name: "probe-user".to_owned(),
    };
    Ok(host.register(client_id, account, Arc::new(store)).await?)
}

/// Waits for the listener to close after the last release, which happens on its own task.
async fn port_released() -> Probe<Duration> {
    for step in 0_u32..40 {
        if TcpStream::connect(("127.0.0.1", DEFAULT_PORT)).await.is_err() {
            return Ok(Duration::from_millis(25) * step);
        }
        sleep(Duration::from_millis(25)).await;
    }
    Err("port 9977 still accepts connections one second after the last release".into())
}

async fn sequential(host: &Arc<Host>, db_path: &Path) -> Probe<()> {
    println!("== two registrations one after the other");
    for (round, game) in [(1, &YS_ORIGIN), (2, &YS_I)] {
        let lease = register(host, db_path, game).await?;
        let mut client = Client::connect().await?;
        let signed_in = client.sign_in(game).await?;
        let (code, count) = client.achievements().await?;
        println!(
            "  round {round}: {} sign-in {signed_in:?}, achievements {code:?} x{count}",
            game.product_id
        );
        drop(lease);
        println!(
            "  round {round}: released, port closed after {:?}",
            port_released().await?
        );
    }
    Ok(())
}

async fn concurrent(host: &Arc<Host>, db_path: &Path) -> Probe<()> {
    println!("== two registrations at once");
    let (origin, ys_i) = tokio::join!(register(host, db_path, &YS_ORIGIN), register(host, db_path, &YS_I));
    let (origin, ys_i) = (origin?, ys_i?);
    let mut origin_client = Client::connect().await?;
    let mut ys_i_client = Client::connect().await?;
    println!(
        "  sign-ins: {} -> {:?}, {} -> {:?}",
        YS_ORIGIN.product_id,
        origin_client.sign_in(&YS_ORIGIN).await?,
        YS_I.product_id,
        ys_i_client.sign_in(&YS_I).await?
    );
    match register(host, db_path, &YS_ORIGIN).await {
        Ok(_lease) => return Err("the same client id was registered twice".into()),
        Err(err) => println!("  {} registered again: refused ({err})", YS_ORIGIN.product_id),
    }
    drop(origin);
    println!(
        "  {} released: its open connection now gets {:?}, {} still gets {:?}",
        YS_ORIGIN.product_id,
        origin_client.achievements().await?,
        YS_I.product_id,
        ys_i_client.achievements().await?
    );
    let mut late = Client::connect().await?;
    println!(
        "  a new connection signing in as the released {} gets {:?}",
        YS_ORIGIN.product_id,
        late.sign_in(&YS_ORIGIN).await?
    );
    drop(ys_i);
    println!(
        "  {} released, port closed after {:?}",
        YS_I.product_id,
        port_released().await?
    );
    Ok(())
}

async fn port_busy(host: &Arc<Host>, db_path: &Path, origin_root: &Path) -> Probe<()> {
    println!("== port 9977 held by another program");
    let squatter = std::net::TcpListener::bind(("127.0.0.1", DEFAULT_PORT))?;
    match register(host, db_path, &YS_ORIGIN).await {
        Ok(_lease) => return Err("registered while another program held the port".into()),
        Err(err) => println!("  Host::register: {err}"),
    }
    let account = ACCOUNT_ID.to_string();
    let served = serve_galaxy(db_path, &account, YS_ORIGIN.product_id, origin_root).await;
    println!("  serve_galaxy (the launch path): lease {}", served.is_some());
    drop(squatter);
    let lease = register(host, db_path, &YS_ORIGIN).await?;
    println!("  port freed: registering again works, nothing was left registered");
    drop(lease);
    println!("  released, port closed after {:?}", port_released().await?);
    Ok(())
}

async fn kill_switch(db_path: &Path, origin_root: &Path, devilution_root: &Path) -> Probe<()> {
    println!("== kill switch and builds without Galaxy");
    let account = ACCOUNT_ID.to_string();
    Db::open(db_path)?.set_galaxy_service_enabled(false)?;
    let served = serve_galaxy(db_path, &account, YS_ORIGIN.product_id, origin_root).await;
    let listening = TcpStream::connect(("127.0.0.1", DEFAULT_PORT)).await.is_ok();
    println!("  switched off: lease {}, port listening {listening}", served.is_some());
    Db::open(db_path)?.set_galaxy_service_enabled(true)?;
    let served = serve_galaxy(db_path, &account, "1308742792", devilution_root).await;
    let listening = TcpStream::connect(("127.0.0.1", DEFAULT_PORT)).await.is_ok();
    println!(
        "  no clientId in goggame info: lease {}, port listening {listening}",
        served.is_some()
    );
    Ok(())
}

async fn run(dir: &Path) -> Probe<()> {
    let db_path = dir.join("decanter.db");
    seed(&db_path)?;
    let origin_root = install(dir, "ys-origin", YS_ORIGIN.product_id, Some(YS_ORIGIN.client_id))?;
    let devilution_root = install(dir, "devilutionx", "1308742792", None)?;
    let host = Host::new(DEFAULT_PORT);
    sequential(&host, &db_path).await?;
    concurrent(&host, &db_path).await?;
    port_busy(&host, &db_path, &origin_root).await?;
    kill_switch(&db_path, &origin_root, &devilution_root).await
}

#[tokio::main]
async fn main() -> Probe<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    let dir = std::env::temp_dir().join(format!("decanter-galaxy-lifecycle-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let result = run(&dir).await;
    std::fs::remove_dir_all(&dir)?;
    println!("removed {}", dir.display());
    result
}
