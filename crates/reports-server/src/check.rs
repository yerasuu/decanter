//! `--check-config` for the two report partitions and the accounts store: one read-only look at each, one line each,
//! no secret in any line.

use crate::config::{Config, Driver, Storage, StorageCredentials};
use crate::store::{self, AccountStore, AuditSummary, Peeked, Section};

const NO_ADMIN: &str = "warning: no admin account, so /admin refuses everyone until `decanter-reports user add \
                        <name> --role admin`";

/// Where a section lives, as the maintainer would look for it.
pub fn describe(storage: &Storage, section: impl Into<Section>) -> String {
    let section = section.into();
    match storage.driver {
        Driver::Local => format!("local directory {}", store::partition_dir(storage, section).display()),
        Driver::S3 | Driver::R2 => format!(
            "bucket {} at {}, prefix {}/",
            storage.bucket,
            if storage.endpoint.is_empty() {
                "the provider"
            } else {
                storage.endpoint.as_str()
            },
            store::partition_prefix(storage, section)
        ),
    }
}

/// What one read-only look at a section found; the accounts store also says how many admins and viewers it holds,
/// how large its audit trail is and how many account objects it cannot read.
enum Status {
    NotCreated,
    NotInitialised,
    Opens(Option<Peeked>),
}

async fn status(storage: &Storage, credentials: &StorageCredentials, section: Section) -> Result<Status, String> {
    if storage.driver == Driver::Local && !store::local_partition_exists(storage, section) {
        return Ok(Status::NotCreated);
    }
    let backend = store::connect(storage, credentials, section).map_err(|error| format!("cannot set up: {error:#}"))?;
    match store::probe(backend.as_ref(), &credentials.passphrase).await {
        Ok(true) if section == Section::Accounts => AccountStore::peek(backend, &credentials.passphrase)
            .await
            .map(|peeked| Status::Opens(Some(peeked)))
            .map_err(|error| format!("{error:#}")),
        Ok(true) => Ok(Status::Opens(None)),
        Ok(false) => Ok(Status::NotInitialised),
        Err(error) => Err(format!("{error:#}")),
    }
}

fn plural(count: u32, one: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {one}s")
    }
}

fn audit_text(audit: &AuditSummary) -> String {
    let count = plural(u32::try_from(audit.records).unwrap_or(u32::MAX), "audit record");
    match &audit.oldest_day {
        Some(day) => format!("{count}, the oldest from {day}"),
        None => count,
    }
}

/// The server keeps such an account only while its name entry vouches for it (`accounts::Accounts::refresh`).
fn unreadable_text(count: usize) -> String {
    match count {
        0 => String::new(),
        count => format!(
            "; warning: {} cannot be read (corrupt, or sealed with another passphrase), see `account object \
             unreadable` in the log",
            plural(u32::try_from(count).unwrap_or(u32::MAX), "account object")
        ),
    }
}

/// One line per section and whether every section passed. A local section the server has not created yet is not
/// opened, because opening it would create it. A section still without encryption is reported as blocked while
/// another one fails, since the server then refuses to create its encryption. A missing admin is a warning on the
/// accounts line, not a failure: the server starts and the CLI adds one.
pub async fn partitions(config: &Config, credentials: &StorageCredentials) -> (Vec<String>, bool) {
    let storage = &config.storage;
    let mut statuses = Vec::with_capacity(Section::ALL.len());
    for section in Section::ALL {
        statuses.push((section, status(storage, credentials, section).await));
    }
    let failed: Vec<&str> = statuses
        .iter()
        .filter(|(_, status)| status.is_err())
        .map(|(section, _)| section.name())
        .collect();
    let first_start = |what: &str| {
        if failed.is_empty() {
            format!("{what} (the first start creates its encryption)")
        } else {
            format!(
                "{what}; the server will not create its encryption while the check of {} fails",
                failed.join(" and ")
            )
        }
    };
    let lines = statuses
        .iter()
        .map(|(section, status)| {
            let mut text = match status {
                Ok(Status::NotCreated) => first_start("not created yet"),
                Ok(Status::NotInitialised) => first_start("reachable, not initialized yet"),
                Ok(Status::Opens(None)) => "reachable, the passphrase opens it".to_owned(),
                Ok(Status::Opens(Some(peeked))) => format!(
                    "reachable, the passphrase opens it; {}, {}; {}{}",
                    plural(peeked.roster.admins, "admin"),
                    plural(peeked.roster.viewers, "viewer"),
                    audit_text(&peeked.audit),
                    unreadable_text(peeked.unreadable)
                ),
                Err(error) => format!("check failed: {error}"),
            };
            let no_admin = matches!(status, Ok(Status::NotCreated | Status::NotInitialised))
                || matches!(status, Ok(Status::Opens(Some(peeked))) if peeked.roster.admins == 0);
            if *section == Section::Accounts && no_admin {
                text = format!("{text}; {NO_ADMIN}");
            }
            match section {
                Section::Reports(product) => {
                    format!("partition {}: {}: {text}", product.name(), describe(storage, *section))
                }
                Section::Accounts => format!("accounts: {}: {text}", describe(storage, *section)),
            }
        })
        .collect();
    (lines, failed.is_empty())
}

/// Secrets in the environment of a long-running process outside a container are readable by every process of the
/// same user (`/proc/<pid>/environ`); inside one, the container is that boundary and the warning would be noise.
pub fn env_secrets_warning(config: &Config, in_container: bool) -> Option<String> {
    let names = config.secrets_from_env();
    (!in_container && !names.is_empty()).then(|| {
        format!(
            "secrets come from the environment ({}) outside a container: every process of this user can read them; \
             prefer the *_file settings (systemd LoadCredential=)",
            names.join(", ")
        )
    })
}

/// What the server does when the proxy's address stops matching `trusted_proxies`, which the check cannot see
/// (it has no peer to compare), so the maintainer knows which log line reveals it.
pub fn proxy_note(config: &Config) -> Option<String> {
    let header = config.client_ip_header.as_deref()?;
    let proxies: Vec<String> = config.trusted_proxies.iter().map(ToString::to_string).collect();
    Some(format!(
        "{header} is read only from {}; if the proxy's address changes, every client is counted as the proxy and \
         the log says `client_ip_header ignored` (at most every 10 minutes)",
        proxies.join(", ")
    ))
}

/// Docker leaves `/.dockerenv` and Podman `/run/.containerenv` in every container they start.
pub fn in_container() -> bool {
    ["/.dockerenv", "/run/.containerenv"]
        .iter()
        .any(|path| std::path::Path::new(path).exists())
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;
    use crate::store::Product;

    fn credentials(passphrase: &str) -> StorageCredentials {
        StorageCredentials {
            passphrase: passphrase.to_owned(),
            bucket_keys: None,
        }
    }

    #[test]
    fn bucket_partitions_sit_under_the_optional_root() {
        let mut storage = Storage {
            driver: Driver::S3,
            bucket: "reports".to_owned(),
            ..Storage::default()
        };
        assert_eq!(
            describe(&storage, Product::Psport),
            "bucket reports at the provider, prefix psport/"
        );
        storage.prefix = "shared/reports".to_owned();
        storage.endpoint = "https://minio.lan:9000".to_owned();
        assert_eq!(
            describe(&storage, Product::Decanter),
            "bucket reports at https://minio.lan:9000, prefix shared/reports/decanter/"
        );
    }

    #[tokio::test]
    async fn each_partition_gets_its_own_line_and_no_secret() -> Result<(), Box<dyn Error>> {
        let dir = std::env::temp_dir().join(format!("decanter-reports-check-{}", crate::secrets::random_hex()?));
        let mut config = Config::default();
        config.storage.dir.clone_from(&dir);
        let right = "the right passphrase, long enough for the check";
        let wrong = "a wrong passphrase, just as long as the right one";

        let (lines, passed) = partitions(&config, &credentials(right)).await;
        assert!(passed);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.contains("not created yet")), "{lines:?}");
        assert!(lines[2].starts_with("accounts: local directory") && lines[2].contains("no admin account"));
        assert!(!dir.exists(), "the check must not create a partition");

        let backend = store::connect(&config.storage, &credentials(right), Product::Decanter)?;
        drop(store::Store::open(backend, right).await?);
        let backend = store::connect(&config.storage, &credentials(right), Section::Accounts)?;
        drop(AccountStore::open(backend, right).await?);
        let (lines, _) = partitions(&config, &credentials(right)).await;
        assert!(
            lines[2].contains("opens it; 0 admins, 0 viewers; 0 audit records;") && lines[2].ends_with(NO_ADMIN),
            "{lines:?}"
        );
        let backend = store::connect(&config.storage, &credentials(right), Section::Accounts)?;
        let accounts = AccountStore::open(backend, right).await?;
        accounts.add("owner", store::Role::Admin, "2026-10-28", false).await?;
        let ana = accounts
            .add("ana", store::Role::Viewer, "2026-10-28", false)
            .await?
            .key()
            .ok_or("ana")?;
        let record = store::AuditRecord {
            user: "owner".to_owned(),
            role: "admin".to_owned(),
            action: "hide".to_owned(),
            ..store::AuditRecord::default()
        };
        accounts.append_audit(&record, 29_000_000).await?;
        accounts.append_audit(&record, 29_100_000).await?;
        drop(accounts);

        let (lines, passed) = partitions(&config, &credentials(right)).await;
        assert!(passed);
        assert!(lines[0].starts_with("partition decanter: local directory") && lines[0].ends_with("opens it"));
        assert!(lines[1].starts_with("partition psport:") && lines[1].contains("not created yet"));
        assert!(
            lines[1].ends_with("(the first start creates its encryption)"),
            "{lines:?}"
        );
        assert!(
            lines[2].ends_with("opens it; 1 admin, 1 viewer; 2 audit records, the oldest from 2025-02-19"),
            "{lines:?}"
        );

        let (lines, passed) = partitions(&config, &credentials(wrong)).await;
        assert!(!passed);
        assert!(
            lines[0].contains("check failed") && lines[2].contains("check failed"),
            "{lines:?}"
        );
        assert!(
            lines[1].ends_with("will not create its encryption while the check of decanter and accounts fails"),
            "{lines:?}"
        );
        assert!(lines.iter().all(|line| !line.contains(right) && !line.contains(wrong)));

        let backend = store::connect(&config.storage, &credentials(right), Section::Accounts)?;
        let key = store::User::COLLECTION.key(ana.get(4..12).unwrap_or_default())?;
        let (_, etag) = backend.get(&key).await?;
        backend.put_cas(&key, b"garbage".to_vec(), etag.as_deref()).await?;
        drop(backend);
        let (lines, _) = partitions(&config, &credentials(right)).await;
        assert!(
            lines[2].contains("audit records, the oldest from 2025-02-19; warning: 1 account object cannot be read"),
            "{lines:?}"
        );
        assert!(!lines[2].contains(&ana), "{lines:?}");
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn the_proxy_note_names_the_trusted_addresses_and_the_drift_line() {
        let mut config = Config::default();
        assert!(proxy_note(&config).is_none());
        config.client_ip_header = Some("cf-connecting-ip".to_owned());
        config.trusted_proxies = vec![std::net::IpAddr::from([10, 0, 0, 2])];
        let note = proxy_note(&config).unwrap_or_default();
        assert!(
            note.starts_with("cf-connecting-ip is read only from 10.0.0.2;"),
            "{note}"
        );
        assert!(note.contains("client_ip_header ignored"), "{note}");
    }

    #[test]
    fn environment_secrets_warn_only_outside_a_container() {
        let mut config = Config::default();
        assert!(env_secrets_warning(&config, true).is_none());
        let warning = env_secrets_warning(&config, false).unwrap_or_default();
        assert!(warning.contains("DECANTER_REPORTS_PASSPHRASE"), "{warning}");
        config.storage.passphrase_file = Some("/etc/decanter-reports/passphrase".into());
        assert!(env_secrets_warning(&config, false).is_none());
    }
}
