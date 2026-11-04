// SPDX-License-Identifier: GPL-3.0-or-later
//! Throwaway hostile-client probe for the local Galaxy service (plan stage S4): the procfs
//! owner parser over crafted tables, every cap and refusal of plan 2.4 over loopback, then random,
//! malformed and huge frames into the codec in-process and into the service over loopback, and
//! last a valid sign-in that must still be served. Not part of the app; no game and no GOG.
//!
//! `cargo run -p decanter-galaxy --example fuzz_probe [port] [fuzz seconds]` (default 19977, 30).
//! `... --example fuzz_probe hold <port> <seconds>` only serves a registered game, for a client
//! started from another uid or user namespace (`unshare`, `setpriv`).

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
#![expect(
    clippy::std_instead_of_alloc,
    reason = "a desktop probe on std, with no no_std target to justify alloc::"
)]
#![expect(
    clippy::separated_literal_suffix,
    reason = "`0_u8` is the workspace's spelling; unseparated_literal_suffix asks the opposite"
)]
#![allow(
    clippy::single_call_fn,
    clippy::large_stack_frames,
    clippy::shadow_unrelated,
    clippy::shadow_reuse,
    clippy::use_debug,
    clippy::default_numeric_fallback,
    clippy::arbitrary_source_item_ordering,
    clippy::big_endian_bytes,
    clippy::too_many_lines,
    clippy::absolute_paths,
    clippy::arithmetic_side_effects,
    clippy::integer_division_remainder_used,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::missing_docs_in_private_items,
    clippy::unseparated_literal_suffix,
    clippy::min_ident_chars,
    clippy::exhaustive_structs,
    clippy::missing_trait_methods,
    clippy::pub_with_shorthand,
    clippy::redundant_pub_crate,
    clippy::host_endian_bytes,
    clippy::needless_pass_by_value,
    clippy::missing_const_for_fn,
    clippy::map_with_unused_argument_over_ranges,
    clippy::cast_sign_loss,
    clippy::pattern_type_mismatch,
    clippy::cognitive_complexity,
    clippy::collection_is_never_read,
    clippy::unused_trait_names,
    dead_code,
    reason = "a throwaway probe that prints what it got; read top to bottom, not linted like the service"
)]

#[path = "../src/peer.rs"]
mod peer;

use core::error::Error;
use core::net::SocketAddr;
use core::num::NonZeroU32;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use std::env;
use std::sync::Arc;
use std::time::Instant;

use decanter_galaxy::proto::communication::{
    AuthInfoRequest, GetUserAchievementsRequest, GetUserAchievementsResponse, MessageType, UnlockUserAchievementRequest,
};
use decanter_galaxy::proto::pb::Header;
use decanter_galaxy::proto::pb::response::Status;
use decanter_galaxy::{
    Account, Achievement, AchievementList, ClientId, FrameError, GameStore, Host, Lease, MAX_PAYLOAD_BYTES,
    ResponseHeader, SourceError, Stat, StatValue, read_frame, write_frame,
};
use prost::Message;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};

type Probe<T> = Result<T, Box<dyn Error + Send + Sync>>;

const GAMES: [&str; 4] = [
    "48766840879051524",
    "49964625586061620",
    "51000000000000001",
    "51000000000000002",
];
const GALAXY_CLIENT_ID: &str = "46899977096215655";
const ACCOUNT_ID: u64 = 46_988_240_917_958_416;
const MAX_RESPONSE_BYTES: u32 = 16 << 20;

const OK: i32 = Status::Ok as i32;

static PANICS: AtomicU64 = AtomicU64::new(0);

/// A fixed list; every write succeeds so that only the service's own limits refuse.
struct Store;

impl GameStore for Store {
    fn achievements(&self, _client_id: &ClientId) -> Result<AchievementList, SourceError> {
        let achievements = (1..=3_u64)
            .map(|id| Achievement {
                achievement_id: Some(5_500 + id),
                key: format!("KEY_{id}"),
                name: format!("Achievement {id}"),
                description: None,
                image_url_locked: None,
                image_url_unlocked: None,
                visible_while_locked: true,
                unlocked_at: None,
                rarity: None,
                rarity_level_description: None,
                rarity_level_slug: None,
            })
            .collect();
        Ok(AchievementList {
            achievements,
            language: None,
            mode: None,
        })
    }

    fn unlock_achievement(&self, _: &ClientId, _: u64, _: NonZeroU32) -> Result<(), SourceError> {
        Ok(())
    }

    fn clear_achievement(&self, _: &ClientId, _: u64) -> Result<(), SourceError> {
        Ok(())
    }

    fn user_stats(&self, _: &ClientId) -> Result<Vec<Stat>, SourceError> {
        Ok(Vec::new())
    }

    fn set_stat(&self, _: &ClientId, _: &str, _: StatValue) -> Result<(), SourceError> {
        Ok(())
    }

    fn seconds_played(&self, _: &ClientId) -> Result<u64, SourceError> {
        Ok(60)
    }
}

/// xorshift64*: enough to scramble bytes, and no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

fn frame_bytes(sort: u32, kind: u32, oseq: u32, payload: &[u8]) -> Vec<u8> {
    let header = Header {
        sort,
        r#type: kind,
        size: Some(payload.len() as u32),
        oseq: Some(oseq),
    }
    .encode_to_vec();
    let mut bytes = (header.len() as u16).to_be_bytes().to_vec();
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(payload);
    bytes
}

fn auth(client_id: &str) -> Vec<u8> {
    AuthInfoRequest {
        client_id: Some(client_id.to_owned()),
        client_secret: Some("0".repeat(64)),
        platform_type: Some(1),
        steam_ticket: None,
        game_pid: Some(4242),
        openid: None,
    }
    .encode_to_vec()
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

    async fn send(&mut self, kind: MessageType, payload: &[u8]) -> Probe<()> {
        self.oseq = self.oseq.wrapping_add(1);
        let header = Header {
            sort: 1,
            r#type: i32::from(kind) as u32,
            size: None,
            oseq: Some(self.oseq),
        };
        write_frame(&mut self.stream, header, payload).await?;
        Ok(())
    }

    /// `None` when the service closed the connection.
    async fn answer(&mut self) -> Probe<Option<(ResponseHeader, Vec<u8>)>> {
        match timeout(
            Duration::from_secs(15),
            read_frame::<ResponseHeader, _>(&mut self.stream, MAX_RESPONSE_BYTES),
        )
        .await
        {
            Ok(Ok(Some(frame))) => Ok(Some((frame.header, frame.payload))),
            Ok(Ok(None) | Err(FrameError::Io(_))) => Ok(None),
            Ok(Err(err)) => Err(err.into()),
            Err(_elapsed) => Err("no answer within 15 s: the service hangs".into()),
        }
    }

    async fn call(&mut self, kind: MessageType, payload: &[u8]) -> Probe<Option<(ResponseHeader, Vec<u8>)>> {
        self.send(kind, payload).await?;
        self.answer().await
    }

    async fn sign_in(port: u16, client_id: &str) -> Probe<(Self, Option<i32>)> {
        let mut client = Self::connect(port).await?;
        let code = client
            .call(MessageType::AuthInfoRequest, &auth(client_id))
            .await?
            .map(|(header, _)| header.code.unwrap_or_default());
        Ok((client, code))
    }

    /// Whether the service closes the connection within `within`.
    async fn closed_within(&mut self, within: Duration) -> bool {
        let mut byte = [0_u8; 1];
        matches!(timeout(within, self.stream.read(&mut byte)).await, Ok(Ok(0) | Err(_)))
    }
}

fn code_name(code: Option<i32>) -> String {
    code.map_or_else(
        || "closed".to_owned(),
        |code| {
            let name = Status::try_from(code).map_or("?", |status| status.as_str_name());
            format!("{code} {name}")
        },
    )
}

fn parser_cases() -> Probe<()> {
    println!("== procfs owner parser over crafted tables");
    let peer: SocketAddr = "127.0.0.1:41000".parse()?;
    let local: SocketAddr = "127.0.0.1:9977".parse()?;
    let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
    // 127.0.0.1 is 0100007F as the kernel prints it on little-endian; 41000 = A028, 9977 = 26F9.
    let line = |local: &str, remote: &str, state: &str, uid: u32| {
        format!(
            "   0: {local} {remote} {state} 00000000:00000000 00:00000000 00000000  {uid}        0 12345 1 0 20 4 30 10 -1\n"
        )
    };
    let client = "0100007F:A028";
    let service = "0100007F:26F9";
    let v4_mapped_client = "0000000000000000FFFF00000100007F:A028";
    let v4_mapped_service = "0000000000000000FFFF00000100007F:26F9";
    let cases: Vec<(&str, String)> = vec![
        ("same user", format!("{header}{}", line(client, service, "01", 1000))),
        ("other user", format!("{header}{}", line(client, service, "01", 1001))),
        (
            "service's own end only",
            format!("{header}{}", line(service, client, "01", 1000)),
        ),
        (
            "TIME_WAIT only (uid 0)",
            format!("{header}{}", line(client, service, "06", 0)),
        ),
        (
            "v4-mapped in tcp6",
            format!("{header}{}", line(v4_mapped_client, v4_mapped_service, "01", 1000)),
        ),
        (
            "ambiguous",
            format!(
                "{header}{}{}",
                line(client, service, "01", 1000),
                line(client, service, "01", 0)
            ),
        ),
        (
            "malformed lines around a good one",
            format!(
                "{header}garbage\n   1: ZZZZZZZZ:26F9 0100007F:A028 01\n   2: 0100007F:A028\n{}   3: 0100007F:FFFFF 0100007F:26F9 01 0 0 0 1000\n",
                line(client, service, "01", 1000)
            ),
        ),
        ("empty table", String::new()),
        (
            "binary noise",
            String::from_utf8_lossy(&Rng(7).bytes(4096)).into_owned(),
        ),
    ];
    for (name, table) in &cases {
        println!("  {name}: {:?}", peer::find_owner(table, peer, local));
    }
    let mut rng = Rng(0x5eed);
    let started = Instant::now();
    let mut lines = 0_u64;
    while started.elapsed() < Duration::from_secs(2) {
        let mut table = line(client, service, "01", 1000).into_bytes();
        for _ in 0..rng.below(8) {
            let at = rng.below(table.len() as u64) as usize;
            table[at] = rng.next() as u8;
        }
        let _owner = peer::find_owner(&String::from_utf8_lossy(&table), peer, local);
        lines += 1;
    }
    println!("  {lines} mutated lines parsed in 2 s without a panic");
    Ok(())
}

async fn codec_in_process(seconds: u64) -> Probe<()> {
    println!("== codec in-process for {seconds} s: random and mutated frames from memory");
    let mut rng = Rng(0xC0DEC);
    let valid = frame_bytes(1, 3, 1, &auth(GAMES[0]));
    let (mut frames, mut errors, mut ends) = (0_u64, 0_u64, 0_u64);
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        let mut input = if rng.below(2) == 0 {
            let bits = rng.below(17);
            let len = rng.below(1 << bits) as usize;
            rng.bytes(len)
        } else {
            let mut mutated = valid.clone();
            for _ in 0..=rng.below(6) {
                let at = rng.below(mutated.len() as u64) as usize;
                mutated[at] = rng.next() as u8;
            }
            mutated
        };
        if rng.below(8) == 0 {
            input.truncate(rng.below(input.len() as u64 + 1) as usize);
        }
        let mut reader = input.as_slice();
        loop {
            match read_frame::<Header, _>(&mut reader, MAX_PAYLOAD_BYTES).await {
                Ok(Some(_)) => frames += 1,
                Ok(None) => {
                    ends += 1;
                    break;
                }
                Err(_) => {
                    errors += 1;
                    break;
                }
            }
        }
    }
    println!(
        "  {frames} frames decoded, {errors} refused, {ends} clean ends, panics {}",
        PANICS.load(Ordering::Relaxed)
    );
    Ok(())
}

async fn refusals(port: u16) -> Probe<()> {
    println!("== allow-list");
    for (name, id) in [
        ("malformed (letters)", "12ab"),
        ("malformed (21 digits)", "123456789012345678901"),
        ("Galaxy's own client id", GALAXY_CLIENT_ID),
        ("unregistered", "99999999999999999"),
        ("registered", GAMES[0]),
    ] {
        let (_client, code) = Client::sign_in(port, id).await?;
        println!("  {name}: {}", code_name(code));
    }
    let mut client = Client::connect(port).await?;
    let code = client
        .call(
            MessageType::GetUserAchievementsRequest,
            &GetUserAchievementsRequest { user_id: None }.encode_to_vec(),
        )
        .await?
        .map(|(header, _)| header.code.unwrap_or_default());
    println!("  achievements before sign-in: {}", code_name(code));

    println!("== frame caps");
    let raw_cases: [(&str, Vec<u8>); 4] = [
        ("header length 0", vec![0, 0]),
        ("header length 257", {
            let mut bytes = vec![1, 1];
            bytes.extend(vec![0_u8; 257]);
            bytes
        }),
        ("header length 0xFFFF", vec![0xFF, 0xFF]),
        ("payload 64 KiB + 1", {
            let header = Header {
                sort: 1,
                r#type: 3,
                size: Some(MAX_PAYLOAD_BYTES + 1),
                oseq: Some(1),
            }
            .encode_to_vec();
            let mut bytes = (header.len() as u16).to_be_bytes().to_vec();
            bytes.extend(header);
            bytes
        }),
    ];
    for (name, bytes) in raw_cases {
        let mut client = Client::connect(port).await?;
        client.stream.write_all(&bytes).await?;
        println!(
            "  {name}: closed {}",
            client.closed_within(Duration::from_secs(2)).await
        );
    }
    let mut client = Client::connect(port).await?;
    client
        .stream
        .write_all(&frame_bytes(1, 3, 1, &vec![0xFF; MAX_PAYLOAD_BYTES as usize]))
        .await?;
    let code = client
        .answer()
        .await?
        .map(|(header, _)| header.code.unwrap_or_default());
    println!("  payload of exactly 64 KiB of 0xFF as AUTH_INFO: {}", code_name(code));

    println!("== timeouts (partial frame 5 s, sign-in 10 s), concurrently");
    let mut partial = Client::connect(port).await?;
    partial.stream.write_all(&[0, 10, 8]).await?;
    let mut idle = Client::connect(port).await?;
    let started = Instant::now();
    let (partial_closed, idle_closed) = tokio::join!(
        async {
            let closed = partial.closed_within(Duration::from_secs(8)).await;
            (closed, started.elapsed())
        },
        async {
            let closed = idle.closed_within(Duration::from_secs(13)).await;
            (closed, started.elapsed())
        }
    );
    println!(
        "  partial frame closed {} after {:?}",
        partial_closed.0, partial_closed.1
    );
    println!(
        "  idle unauthenticated closed {} after {:?}",
        idle_closed.0, idle_closed.1
    );

    println!("== connection caps");
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(Client::connect(port).await?);
    }
    sleep(Duration::from_millis(200)).await;
    let mut fifth = Client::connect(port).await?;
    println!(
        "  5th unauthenticated connection closed at once: {}",
        fifth.closed_within(Duration::from_secs(1)).await
    );
    held.clear();
    sleep(Duration::from_millis(200)).await;
    let mut signed = Vec::new();
    for _ in 0..4 {
        let (client, code) = Client::sign_in(port, GAMES[1]).await?;
        if code != Some(OK) {
            println!("  sign-in {} answered {}", signed.len() + 1, code_name(code));
        }
        signed.push(client);
    }
    let (fifth, code) = Client::sign_in(port, GAMES[1]).await?;
    println!("  5th sign-in of one game: {}", code_name(code));
    drop(fifth);
    sleep(Duration::from_millis(200)).await;
    for game in [GAMES[2], GAMES[3], GAMES[0]] {
        for _ in 0..4 {
            signed.push(Client::sign_in(port, game).await?.0);
        }
    }
    let (_spare, code) = Client::sign_in(port, GAMES[0]).await?;
    println!("  {} signed in; one more connection: {}", signed.len(), code_name(code));
    drop(signed);
    sleep(Duration::from_millis(300)).await;

    println!("== request rate (200/s, burst 400) on one connection");
    let (mut client, _) = Client::sign_in(port, GAMES[0]).await?;
    let started = Instant::now();
    let sent = 1_000;
    for _ in 0..sent {
        client.send(MessageType::LibraryInfoRequest, &[]).await?;
    }
    let (mut busy, mut answered, mut retry_after) = (0, 0, None);
    for _ in 0..sent {
        let Some((header, _)) = client.answer().await? else {
            break;
        };
        answered += 1;
        if header.code == Some(i32::from(Status::Unavailable)) {
            busy += 1;
            retry_after = header.retry_after;
        }
    }
    println!(
        "  {sent} pipelined: {answered} answered, {busy} UNAVAILABLE (retry_after {retry_after:?}), in {:?}",
        started.elapsed()
    );

    println!("== write rate (20/s, burst 100) of one game");
    sleep(Duration::from_secs(3)).await;
    let (mut client, _) = Client::sign_in(port, GAMES[0]).await?;
    let started = Instant::now();
    let unlock = UnlockUserAchievementRequest {
        achievement_id: Some(5_501),
        time: None,
    }
    .encode_to_vec();
    let writes = 200;
    for _ in 0..writes {
        client.send(MessageType::UnlockUserAchievementRequest, &unlock).await?;
    }
    let (mut ok, mut busy) = (0, 0);
    for _ in 0..writes {
        let Some((header, _)) = client.answer().await? else {
            break;
        };
        match header.code {
            Some(OK) => ok += 1,
            Some(code) if code == i32::from(Status::Unavailable) => busy += 1,
            _ => {}
        }
    }
    println!(
        "  {writes} unlocks pipelined: {ok} stored, {busy} UNAVAILABLE, in {:?}",
        started.elapsed()
    );
    Ok(())
}

async fn fuzz_worker(port: u16, seed: u64, until: Instant) -> (u64, u64, u64) {
    let mut rng = Rng(seed);
    let (mut connections, mut frames, mut refused) = (0_u64, 0_u64, 0_u64);
    while Instant::now() < until {
        let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await else {
            refused += 1;
            continue;
        };
        connections += 1;
        let mut oseq = 0;
        if rng.below(2) == 0 {
            let _sent = stream.write_all(&frame_bytes(1, 3, oseq, &auth(GAMES[3]))).await;
        }
        for _ in 0..rng.below(64) {
            oseq += 1;
            let bytes = match rng.below(4) {
                0 => {
                    let bits = rng.below(17);
                    let len = rng.below(1 << bits) as usize;
                    rng.bytes(len)
                }
                1 => {
                    let len = rng.below(512) as usize;
                    frame_bytes(rng.below(4) as u32, rng.below(90) as u32, oseq, &rng.bytes(len))
                }
                2 => {
                    let mut frame = frame_bytes(1, rng.below(90) as u32, oseq, &auth(GAMES[3]));
                    let at = rng.below(frame.len() as u64) as usize;
                    frame[at] = rng.next() as u8;
                    frame
                }
                _ => {
                    let len = rng.below(64) as usize;
                    let payload = rng.bytes(len);
                    frame_bytes(1, [15, 17, 23, 25, 27, 43][rng.below(6) as usize], oseq, &payload)
                }
            };
            frames += 1;
            if stream.write_all(&bytes).await.is_err() {
                break;
            }
        }
        let mut sink = vec![0_u8; 1 << 16];
        let deadline = Duration::from_millis(50 + rng.below(200));
        while let Ok(Ok(read)) = timeout(deadline, stream.read(&mut sink)).await {
            if read == 0 {
                break;
            }
        }
    }
    (connections, frames, refused)
}

async fn healthy(port: u16) -> Probe<()> {
    let (mut client, code) = Client::sign_in(port, GAMES[0]).await?;
    let answer = client
        .call(
            MessageType::GetUserAchievementsRequest,
            &GetUserAchievementsRequest { user_id: None }.encode_to_vec(),
        )
        .await?;
    let Some((header, payload)) = answer else {
        return Err("the service closed a valid session".into());
    };
    let list = GetUserAchievementsResponse::decode(payload.as_slice())?;
    println!(
        "  sign-in {}, achievements {} with {} entries",
        code_name(code),
        code_name(header.code),
        list.user_achievements.len()
    );
    if code != Some(OK) || header.code != Some(OK) || list.user_achievements.len() != 3 {
        return Err("the service is not healthy".into());
    }
    Ok(())
}

async fn register_all(host: &Arc<Host>) -> Probe<Vec<Lease>> {
    let mut leases = Vec::new();
    for game in GAMES {
        let account = Account {
            user_id: ACCOUNT_ID,
            user_name: "probe".to_owned(),
        };
        let client_id = ClientId::parse(game).ok_or("the probe's client id is not valid")?;
        leases.push(host.register(client_id, account, Arc::new(Store)).await?);
    }
    Ok(leases)
}

#[tokio::main]
async fn main() -> Probe<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICS.fetch_add(1, Ordering::Relaxed);
        default_hook(info);
    }));
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("hold") {
        let port = args.get(1).ok_or("hold <port> <seconds>")?.parse()?;
        let seconds = args.get(2).ok_or("hold <port> <seconds>")?.parse()?;
        let host = Host::new(port);
        let leases = register_all(&host).await?;
        println!("holding 127.0.0.1:{port} for {seconds} s");
        sleep(Duration::from_secs(seconds)).await;
        drop(leases);
        sleep(Duration::from_millis(200)).await;
        return Ok(());
    }
    let port = args.first().map_or(Ok(19_977), |text| text.parse())?;
    let fuzz_seconds: u64 = args.get(1).map_or(Ok(30), |text| text.parse())?;

    parser_cases()?;
    codec_in_process(fuzz_seconds.min(10)).await?;

    let host = Host::new(port);
    let leases = register_all(&host).await?;
    println!("service on 127.0.0.1:{port}");
    refusals(port).await?;

    println!("== loopback fuzz for {fuzz_seconds} s, 3 workers");
    let started = Instant::now();
    let until = started + Duration::from_secs(fuzz_seconds);
    let mut workers = JoinSet::new();
    for seed in 1..=3_u64 {
        workers.spawn(fuzz_worker(port, seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), until));
    }
    let (mut connections, mut frames, mut refused) = (0, 0, 0);
    while let Some(done) = workers.join_next().await {
        let (c, f, r) = done?;
        connections += c;
        frames += f;
        refused += r;
    }
    println!(
        "  {connections} connections, {frames} hostile frames, {refused} connects refused, in {:?}",
        started.elapsed()
    );
    sleep(Duration::from_millis(300)).await;

    println!("== health after everything");
    healthy(port).await?;
    println!("panics seen: {}", PANICS.load(Ordering::Relaxed));
    drop(leases);
    sleep(Duration::from_millis(300)).await;
    if PANICS.load(Ordering::Relaxed) > 0 {
        return Err("a panic happened".into());
    }
    Ok(())
}
