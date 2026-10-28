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

/// One line per partition and whether every partition passed. A local partition the server has not created yet is
/// not opened, because opening it would create it.
pub async fn partitions(config: &Config, credentials: &StorageCredentials) -> (Vec<String>, bool) {
    let storage = &config.storage;
    let mut lines = Vec::with_capacity(Product::ALL.len());
    let mut passed = true;
    for product in Product::ALL {
        let status = if storage.driver == Driver::Local && !store::local_partition_exists(storage, product) {
            Ok("not created yet (the first start creates it and its encryption)")
        } else {
            match store::connect(storage, credentials, product) {
                Ok(backend) => match store::probe(backend.as_ref(), &credentials.passphrase).await {
                    Ok(true) => Ok("reachable, the passphrase opens it"),
                    Ok(false) => Ok("reachable, not initialized yet (the first start creates its encryption)"),
                    Err(error) => Err(format!("{error:#}")),
                },
                Err(error) => Err(format!("cannot set up: {error:#}")),
            }
        };
        let status = status.map_or_else(
            |error| {
                passed = false;
                format!("check failed: {error}")
            },
            str::to_owned,
        );
        lines.push(format!(
            "partition {}: {}: {status}",
            product.name(),
            describe(storage, product)
        ));
    }
    (lines, passed)
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

        let (lines, passed) = partitions(&config, &credentials(wrong)).await;
        assert!(!passed);
        assert!(lines[0].contains("check failed"), "{lines:?}");
        assert!(lines.iter().all(|line| !line.contains(right) && !line.contains(wrong)));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
