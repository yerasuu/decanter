//! `--check-config` for the two partitions: one read-only look at each, one line each, no secret in any line.

use crate::config::{Config, Driver, Storage, StorageCredentials};
use crate::store::{self, Product};

/// Where a partition lives, as the maintainer would look for it.
pub fn describe(storage: &Storage, product: Product) -> String {
    match storage.driver {
        Driver::Local => format!("local directory {}", store::partition_dir(storage, product).display()),
        Driver::S3 | Driver::R2 => format!(
            "bucket {} at {}, prefix {}/",
            storage.bucket,
            if storage.endpoint.is_empty() {
                "the provider"
            } else {
                storage.endpoint.as_str()
            },
            store::partition_prefix(storage, product)
        ),
    }
}

/// What one read-only look at a partition found.
enum Status {
    NotCreated,
    NotInitialised,
    Opens,
}

async fn status(storage: &Storage, credentials: &StorageCredentials, product: Product) -> Result<Status, String> {
    if storage.driver == Driver::Local && !store::local_partition_exists(storage, product) {
        return Ok(Status::NotCreated);
    }
    let backend = store::connect(storage, credentials, product).map_err(|error| format!("cannot set up: {error:#}"))?;
    match store::probe(backend.as_ref(), &credentials.passphrase).await {
        Ok(true) => Ok(Status::Opens),
        Ok(false) => Ok(Status::NotInitialised),
        Err(error) => Err(format!("{error:#}")),
    }
}

/// One line per partition and whether every partition passed. A local partition the server has not created yet is
/// not opened, because opening it would create it. A partition still without encryption is reported as blocked
/// while another one fails, since the server then refuses to create its encryption.
pub async fn partitions(config: &Config, credentials: &StorageCredentials) -> (Vec<String>, bool) {
    let storage = &config.storage;
    let mut statuses = Vec::with_capacity(Product::ALL.len());
    for product in Product::ALL {
        statuses.push((product, status(storage, credentials, product).await));
    }
    let failed: Vec<&str> = statuses
        .iter()
        .filter(|(_, status)| status.is_err())
        .map(|(product, _)| product.name())
        .collect();
    let first_start = |what: &str| {
        if failed.is_empty() {
            format!("{what} (the first start creates its encryption)")
        } else {
            format!(
                "{what}; the server will not create its encryption while the {} partition fails its check",
                failed.join(" and ")
            )
        }
    };
    let lines = statuses
        .iter()
        .map(|(product, status)| {
            let status = match status {
                Ok(Status::NotCreated) => first_start("not created yet"),
                Ok(Status::NotInitialised) => first_start("reachable, not initialized yet"),
                Ok(Status::Opens) => "reachable, the passphrase opens it".to_owned(),
                Err(error) => format!("check failed: {error}"),
            };
            format!(
                "partition {}: {}: {status}",
                product.name(),
                describe(storage, *product)
            )
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
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.contains("not created yet")), "{lines:?}");
        assert!(!dir.exists(), "the check must not create a partition");

        let backend = store::connect(&config.storage, &credentials(right), Product::Decanter)?;
        drop(store::Store::open(backend, right).await?);

        let (lines, passed) = partitions(&config, &credentials(right)).await;
        assert!(passed);
        assert!(lines[0].starts_with("partition decanter: local directory") && lines[0].ends_with("opens it"));
        assert!(lines[1].starts_with("partition psport:") && lines[1].contains("not created yet"));
        assert!(
            lines[1].ends_with("(the first start creates its encryption)"),
            "{lines:?}"
        );

        let (lines, passed) = partitions(&config, &credentials(wrong)).await;
        assert!(!passed);
        assert!(lines[0].contains("check failed"), "{lines:?}");
        assert!(
            lines[1].ends_with("will not create its encryption while the decanter partition fails its check"),
            "{lines:?}"
        );
        assert!(lines.iter().all(|line| !line.contains(right) && !line.contains(wrong)));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
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
