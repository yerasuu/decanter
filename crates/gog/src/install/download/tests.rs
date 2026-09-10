use super::*;

fn zlib(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn inflates_what_gog_ships() {
    assert_eq!(inflate(&zlib(b"hello world")).unwrap(), b"hello world");
}

#[test]
fn refuses_bytes_that_are_not_zlib() {
    assert!(inflate(b"not compressed at all").is_err());
}

#[test]
fn a_fresh_cancel_is_not_cancelled() {
    let cancel = Cancel::default();
    assert!(!cancel.is_cancelled());
    assert!(cancel.check().is_ok());
}

/// Clones share one flag — the whole point, since the cancel arrives
/// on a different task from the ones downloading.
#[test]
fn cancelling_one_clone_cancels_them_all() {
    let cancel = Cancel::default();
    let clone = cancel.clone();
    clone.cancel();
    assert!(cancel.is_cancelled());
    assert!(cancel.check().is_err());
}

/// A stand-in for GOG's CDN: serves one zlib chunk per path, and
/// counts how many times each was asked for so the retry path can be
/// checked. `fail_first` paths return 500 once before succeeding.
mod cdn {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    pub(super) struct Cdn {
        pub(super) port: u16,
        pub(super) hits: Arc<AtomicUsize>,
    }

    /// Minimal HTTP/1.1: read the request line, answer, close. Enough
    /// for reqwest, and far less than pulling in a server crate.
    pub(super) async fn spawn(bodies: HashMap<String, Vec<u8>>, fail_first: bool) -> Cdn {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let bodies = bodies.clone();
                let counter = Arc::clone(&counter);
                tokio::spawn(async move {
                    let mut buffer = [0u8; 1024];
                    let Ok(read) = socket.read(&mut buffer).await else {
                        return;
                    };
                    let request = String::from_utf8_lossy(&buffer[..read]);
                    let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();

                    let hit = counter.fetch_add(1, Ordering::SeqCst);
                    let response = match bodies.get(&path) {
                        Some(_) if fail_first && hit == 0 => {
                            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                                .to_vec()
                        }
                        Some(body) => {
                            let mut response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            )
                            .into_bytes();
                            response.extend_from_slice(body);
                            response
                        }
                        None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                    };
                    let _ = socket.write_all(&response).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        Cdn { port, hits }
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join("decanter-download-tests").join(name);
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// The whole contract in one test: chunks are inflated, concatenated
/// in order, and written to a path that follows the manifest's own
/// directory structure.
#[tokio::test]
async fn writes_chunks_inflated_and_in_order() {
    let bodies = std::collections::HashMap::from([
        ("/a".to_string(), zlib(b"first ")),
        ("/b".to_string(), zlib(b"second ")),
        ("/c".to_string(), zlib(b"third")),
    ]);
    let cdn = cdn::spawn(bodies, false).await;
    let staging = scratch("in-order");

    let file = PlannedDownload {
        relative_path: "Game_Data/payload.bin".to_string(),
        chunks: ["a", "b", "c"]
            .iter()
            .map(|name| ChunkSource {
                url: Url::parse(&format!("http://127.0.0.1:{}/{name}", cdn.port)).unwrap(),
                compressed: true,
            })
            .collect(),
    };

    let (tx, mut rx) = mpsc::channel(32);
    let collected = tokio::spawn(async move {
        let mut updates = Vec::new();
        while let Some(update) = rx.recv().await {
            updates.push(update);
        }
        updates
    });

    let written = download_all(vec![file], &staging, Cancel::default(), tx)
        .await
        .expect("download should succeed");

    assert_eq!(written, vec![18]);
    assert_eq!(
        std::fs::read_to_string(staging.join("Game_Data/payload.bin")).unwrap(),
        "first second third"
    );

    // Progress is cumulative per file, one update per chunk.
    let updates = collected.await.unwrap();
    assert_eq!(
        updates.iter().map(|(_, _, bytes)| *bytes).collect::<Vec<_>>(),
        vec![6, 13, 18]
    );
}

/// An uncompressed chunk is written through untouched — the flag is
/// per chunk, not per file.
#[tokio::test]
async fn passes_an_uncompressed_chunk_through() {
    let bodies = std::collections::HashMap::from([("/raw".to_string(), b"plain bytes".to_vec())]);
    let cdn = cdn::spawn(bodies, false).await;
    let staging = scratch("uncompressed");

    let file = PlannedDownload {
        relative_path: "raw.bin".to_string(),
        chunks: vec![ChunkSource {
            url: Url::parse(&format!("http://127.0.0.1:{}/raw", cdn.port)).unwrap(),
            compressed: false,
        }],
    };

    let (tx, _rx) = mpsc::channel(32);
    download_all(vec![file], &staging, Cancel::default(), tx).await.unwrap();
    assert_eq!(std::fs::read(staging.join("raw.bin")).unwrap(), b"plain bytes");
}

/// A dropped connection is the normal failure here, so one 500 must
/// not cost the install.
#[tokio::test]
async fn retries_a_chunk_the_cdn_refused_once() {
    let bodies = std::collections::HashMap::from([("/c".to_string(), zlib(b"recovered"))]);
    let cdn = cdn::spawn(bodies, true).await;
    let staging = scratch("retry");

    let file = PlannedDownload {
        relative_path: "flaky.bin".to_string(),
        chunks: vec![ChunkSource {
            url: Url::parse(&format!("http://127.0.0.1:{}/c", cdn.port)).unwrap(),
            compressed: true,
        }],
    };

    let (tx, _rx) = mpsc::channel(32);
    download_all(vec![file], &staging, Cancel::default(), tx)
        .await
        .expect("a single 500 should be retried, not fatal");
    assert_eq!(std::fs::read_to_string(staging.join("flaky.bin")).unwrap(), "recovered");
    assert!(cdn.hits.load(std::sync::atomic::Ordering::SeqCst) >= 2);
}

/// The install-wide cancel: set before anything starts, nothing is
/// fetched and the call fails rather than quietly finishing.
#[tokio::test]
async fn a_cancelled_install_downloads_nothing() {
    let bodies = std::collections::HashMap::from([("/a".to_string(), zlib(b"never"))]);
    let cdn = cdn::spawn(bodies, false).await;
    let staging = scratch("cancelled");

    let file = PlannedDownload {
        relative_path: "nope.bin".to_string(),
        chunks: vec![ChunkSource {
            url: Url::parse(&format!("http://127.0.0.1:{}/a", cdn.port)).unwrap(),
            compressed: true,
        }],
    };

    let cancel = Cancel::default();
    cancel.cancel();
    let (tx, _rx) = mpsc::channel(32);
    let result = download_all(vec![file], &staging, cancel, tx).await;

    assert!(result.is_err(), "a cancelled install must not report success");
    assert_eq!(cdn.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// One bad file fails the whole install: a partial install is not a
/// smaller install, and the caller throws staging away on this.
#[tokio::test]
async fn one_missing_file_fails_the_install() {
    let bodies = std::collections::HashMap::from([("/ok".to_string(), zlib(b"fine"))]);
    let cdn = cdn::spawn(bodies, false).await;
    let staging = scratch("one-fails");

    let files = vec![
        PlannedDownload {
            relative_path: "ok.bin".to_string(),
            chunks: vec![ChunkSource {
                url: Url::parse(&format!("http://127.0.0.1:{}/ok", cdn.port)).unwrap(),
                compressed: true,
            }],
        },
        PlannedDownload {
            relative_path: "gone.bin".to_string(),
            chunks: vec![ChunkSource {
                url: Url::parse(&format!("http://127.0.0.1:{}/missing", cdn.port)).unwrap(),
                compressed: true,
            }],
        },
    ];

    let (tx, _rx) = mpsc::channel(32);
    let result = download_all(files, &staging, Cancel::default(), tx).await;
    assert!(result.is_err());
}
