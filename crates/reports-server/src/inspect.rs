//! `decanter-reports inspect <product> ls [prefix] [limit] | get <key>`: lists and decrypts the objects of one
//! partition for the maintainer. `s3db-inspect` cannot do it, because each object is sealed with its key inside the
//! partition (the prefix layer is this server's) as associated data. It only reads: the cipher is opened, never
//! created, and a local partition that does not exist yet is not opened.

use std::fmt::Write as _;
use std::io::Write as _;

use crate::config::{Config, Driver, StorageCredentials};
use crate::store::{self, Product};

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1000;

pub const USAGE: &str = "usage: decanter-reports inspect <decanter|psport> ls [prefix] [limit] | get <key> \
                         (the config path comes from DECANTER_REPORTS_CONFIG)";

pub enum Command {
    List { prefix: String, limit: usize },
    Get { key: String },
}

pub struct Request {
    product: Product,
    command: Command,
}

/// Keys are the maintainer's own input, but they still go to the bucket: printable ASCII, no spaces.
fn is_key(text: &str) -> bool {
    text.bytes().all(|b| b.is_ascii_graphic())
}

/// # Errors
///
/// The arguments do not have one of the accepted shapes; the message is the usage line.
pub fn parse(args: &[String]) -> Result<Request, String> {
    let usage = || USAGE.to_owned();
    let mut args = args.iter().map(String::as_str);
    let product = args.next().and_then(Product::from_name).ok_or_else(usage)?;
    let command = match (args.next(), args.next(), args.next(), args.next()) {
        (Some("ls"), prefix, limit, None) => {
            let prefix = prefix.unwrap_or_default();
            let limit = limit
                .map_or(Ok(DEFAULT_LIMIT), str::parse::<usize>)
                .map_err(|_| usage())?;
            if !is_key(prefix) {
                return Err(usage());
            }
            Command::List {
                prefix: prefix.to_owned(),
                limit: limit.clamp(1, MAX_LIMIT),
            }
        }
        (Some("get"), Some(key), None, None) if !key.is_empty() && is_key(key) => Command::Get { key: key.to_owned() },
        _ => return Err(usage()),
    };
    Ok(Request { product, command })
}

/// # Errors
///
/// The partition does not exist yet, cannot be reached, the passphrase does not open it, the key holds nothing, or
/// standard output is closed.
pub async fn run(config: &Config, credentials: &StorageCredentials, request: &Request) -> Result<(), String> {
    let storage = &config.storage;
    let name = request.product.name();
    if storage.driver == Driver::Local && !store::local_partition_exists(storage, request.product) {
        return Err(format!("the {name} partition does not exist yet"));
    }
    let backend = store::connect(storage, credentials, request.product)
        .map_err(|error| format!("cannot set up the {name} partition: {error:#}"))?;
    let cipher = s3db::bootstrap::open_cipher(backend.as_ref(), &credentials.passphrase)
        .await
        .map_err(|error| format!("cannot open the {name} partition: {error:#}"))?;
    let text = match &request.command {
        Command::List { prefix, limit } => {
            let page = backend
                .list_page(prefix, None, *limit)
                .await
                .map_err(|error| format!("cannot list {prefix:?}: {error:#}"))?;
            let mut text = String::new();
            for (key, size) in &page.entries {
                let _ = writeln!(text, "{key}\t{size}");
            }
            if let Some(next) = page.next_after {
                let _ = writeln!(text, "(more after {next})");
            }
            text
        }
        Command::Get { key } => {
            let (value, etag) = s3db::get_json::<serde_json::Value>(backend.as_ref(), key, &cipher)
                .await
                .map_err(|error| format!("cannot read {key}: {error:#}"))?;
            if etag.is_none() {
                return Err(format!("no object at {key}"));
            }
            let text = serde_json::to_string_pretty(&value).map_err(|error| format!("cannot print {key}: {error}"))?;
            format!("{text}\n")
        }
    };
    std::io::stdout()
        .lock()
        .write_all(text.as_bytes())
        .map_err(|error| format!("cannot write the output: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn accepts_the_two_commands_and_nothing_else() {
        assert!(parse(&args("psport ls")).is_ok());
        assert!(parse(&args("decanter ls groups/ 20")).is_ok());
        assert!(parse(&args("decanter get meta/totals.json")).is_ok());
        for bad in [
            "",
            "other ls",
            "decanter",
            "decanter rm x",
            "decanter get",
            "decanter ls a 10 extra",
            "decanter ls a many",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
    }
}
