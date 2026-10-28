//! `decanter-reports user add|list|remove|reset|role`: the maintainers' accounts, managed with the binary so the
//! first admin needs no shell on the host. Only `add` and `reset` write to standard output, and only the key, so
//! `$(decanter-reports user add ...)` captures it whole; everything else goes to standard error. A key is never
//! logged and `list` never shows a hash.

use std::fmt::Write as _;
use std::io::Write;

use crate::store::{AccountChange, AccountStore, NAME_RULE, Role, is_name};

pub const USAGE: &str = "usage: decanter-reports user add <name> --role viewer|admin | list | remove <name> | \
                         reset <name> | role <name> --role viewer|admin (the config path comes from \
                         DECANTER_REPORTS_CONFIG)";

pub enum Command {
    Add { name: String, role: Role },
    List,
    Remove { name: String },
    Reset { name: String },
    Role { name: String, role: Role },
}

fn name(text: &str) -> Result<String, String> {
    if is_name(text) {
        Ok(text.to_owned())
    } else {
        Err(format!("a user name is {NAME_RULE}"))
    }
}

fn role(flag: &str, value: &str) -> Result<Role, String> {
    if flag != "--role" {
        return Err(USAGE.to_owned());
    }
    Role::from_name(value).ok_or_else(|| "--role is viewer or admin".to_owned())
}

/// # Errors
///
/// The arguments do not have one of the accepted shapes, or a name or role is not valid.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["add", who, flag, value] => Ok(Command::Add {
            name: name(who)?,
            role: role(flag, value)?,
        }),
        ["list"] => Ok(Command::List),
        ["remove", who] => Ok(Command::Remove { name: name(who)? }),
        ["reset", who] => Ok(Command::Reset { name: name(who)? }),
        ["role", who, flag, value] => Ok(Command::Role {
            name: name(who)?,
            role: role(flag, value)?,
        }),
        _ => Err(USAGE.to_owned()),
    }
}

fn written(result: std::io::Result<()>) -> Result<(), String> {
    result.map_err(|error| format!("cannot write the output: {error}"))
}

fn refused(change: &AccountChange, name: &str) -> Result<(), String> {
    match change {
        AccountChange::Done | AccountChange::Key(_) => Ok(()),
        AccountChange::NoSuchUser => Err(format!("no user named {name}")),
        AccountChange::LastAdmin => Err(format!(
            "{name} is the last admin: add another admin first, so the server keeps one"
        )),
    }
}

fn show_key(out: &mut impl Write, notes: &mut impl Write, key: &str, what: &str) -> Result<(), String> {
    written(out.write_all(format!("{key}\n").as_bytes()))?;
    written(writeln!(
        notes,
        "{what}; its key is on standard output and is shown only now (the server keeps only its hash)"
    ))
}

fn table(users: &[crate::store::User]) -> String {
    let mut text = String::from("name\trole\tcreated\tlast login\tstate\n");
    for user in users {
        let _ = writeln!(
            text,
            "{}\t{}\t{}\t{}\t{}",
            user.name,
            user.role.name(),
            user.created_day,
            user.last_login_day.as_deref().unwrap_or("never"),
            if user.disabled { "disabled" } else { "active" }
        );
    }
    text
}

/// Runs one command against an open account store; `out` gets only a new key or the list, `notes` the rest.
///
/// # Errors
///
/// The store fails, the account does not exist, the change would leave no admin, the name is taken, or an output
/// is closed.
pub async fn run(
    store: &AccountStore,
    command: &Command,
    out: &mut impl Write,
    notes: &mut impl Write,
) -> Result<(), String> {
    let failed = |error: anyhow::Error| format!("the accounts store failed: {error:#}");
    match command {
        Command::Add { name, role } => {
            let today = crate::app::today();
            let Some(key) = store.add(name, *role, &today).await.map_err(failed)? else {
                return Err(format!("a user named {name} already exists"));
            };
            show_key(out, notes, &key, &format!("user {name} added as {}", role.name()))
        }
        Command::List => {
            let users = store.users().await.map_err(failed)?;
            written(out.write_all(table(&users).as_bytes()))?;
            if !users.iter().any(|user| user.role == Role::Admin && user.is_active()) {
                written(writeln!(
                    notes,
                    "warning: no admin account: /admin refuses everyone until `decanter-reports user add <name> \
                     --role admin`"
                ))?;
            }
            Ok(())
        }
        Command::Remove { name } => {
            refused(&store.remove(name).await.map_err(failed)?, name)?;
            written(writeln!(
                notes,
                "user {name} removed; its key and sessions stop working within 15 s"
            ))
        }
        Command::Reset { name } => match store.reset(name).await.map_err(failed)? {
            AccountChange::Key(key) => show_key(
                out,
                notes,
                &key,
                &format!("user {name} has a new key; the old one and its sessions stop working within 15 s"),
            ),
            other => refused(&other, name),
        },
        Command::Role { name, role } => {
            refused(&store.set_role(name, *role).await.map_err(failed)?, name)?;
            written(writeln!(
                notes,
                "user {name} is now {}; open sessions follow within 15 s",
                role.name()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;
    use crate::store::test_accounts;

    type Test = Result<(), Box<dyn Error>>;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn accepts_the_five_commands_and_nothing_else() {
        for good in [
            "add owner --role admin",
            "add ana-2 --role viewer",
            "list",
            "remove owner",
            "reset owner",
            "role owner --role viewer",
        ] {
            assert!(parse(&args(good)).is_ok(), "{good}");
        }
        for bad in [
            "",
            "add owner",
            "add owner --role root",
            "add Owner --role admin",
            "add owner --admin admin",
            "list all",
            "remove",
            "reset a b",
            "role owner admin",
            "delete owner",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
    }

    async fn run_text(store: &AccountStore, line: &str) -> (Result<(), String>, String, String) {
        let mut out = Vec::new();
        let mut notes = Vec::new();
        let result = match parse(&args(line)) {
            Ok(command) => run(store, &command, &mut out, &mut notes).await,
            Err(usage) => Err(usage),
        };
        (
            result,
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&notes).into_owned(),
        )
    }

    #[tokio::test]
    async fn only_add_and_reset_print_a_key_and_list_shows_no_hash() -> Test {
        let dir = std::env::temp_dir().join(format!("decanter-reports-cli-{}", crate::secrets::random_hex()?));
        let store = test_accounts(&dir);
        let (result, key, notes) = run_text(&store, "add owner --role admin").await;
        assert!(result.is_ok(), "{result:?}");
        assert!(key.starts_with("drk_") && key.ends_with('\n') && key.lines().count() == 1);
        let key = key.trim_end().to_owned();
        assert!(!notes.contains(&key));
        let (result, _, _) = run_text(&store, "add owner --role viewer").await;
        assert_eq!(result, Err("a user named owner already exists".to_owned()));
        run_text(&store, "add ana --role viewer").await.0?;

        let (result, list, notes) = run_text(&store, "list").await;
        result?;
        assert!(
            list.contains("owner\tadmin\t") && list.contains("ana\tviewer\t"),
            "{list}"
        );
        assert!(notes.is_empty(), "{notes}");
        let hashes: Vec<String> = store.users().await?.into_iter().map(|user| user.key_hash).collect();
        assert!(hashes.iter().all(|hash| !list.contains(hash.as_str())));
        assert!(!list.contains(&key) && !list.contains("drk_"));

        let (result, new_key, _) = run_text(&store, "reset owner").await;
        result?;
        let new_key = new_key.trim_end();
        assert!(new_key.starts_with(key.get(..12).unwrap_or_default()), "same id");
        assert_ne!(new_key, key);
        for quiet in ["role ana --role admin", "role ana --role viewer", "list"] {
            let (result, out, notes) = run_text(&store, quiet).await;
            result?;
            assert!(!out.contains("drk_") && !notes.contains("drk_"), "{quiet}");
        }
        assert!(run_text(&store, "remove nobody").await.0.is_err());
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn remove_and_role_refuse_to_leave_no_admin() -> Test {
        let dir = std::env::temp_dir().join(format!("decanter-reports-cli-{}", crate::secrets::random_hex()?));
        let store = test_accounts(&dir);
        run_text(&store, "add owner --role admin").await.0?;
        run_text(&store, "add ana --role viewer").await.0?;
        for refused in ["remove owner", "role owner --role viewer"] {
            let (result, ..) = run_text(&store, refused).await;
            assert!(result.is_err_and(|message| message.contains("last admin")), "{refused}");
        }
        run_text(&store, "role ana --role admin").await.0?;
        run_text(&store, "role owner --role viewer").await.0?;
        run_text(&store, "remove owner").await.0?;
        assert!(run_text(&store, "remove ana").await.0.is_err());
        let roster = store.roster().await?;
        assert_eq!((roster.admins, roster.viewers), (1, 0));
        let (result, ..) = run_text(&store, "add owner --role viewer").await;
        assert!(result.is_ok(), "a removed name is free again: {result:?}");
        let (_, list, notes) = run_text(&store, "list").await;
        assert!(list.contains("owner\tviewer") && notes.is_empty(), "{list}");
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn two_adds_of_one_name_leave_exactly_one_account() -> Test {
        let dir = std::env::temp_dir().join(format!("decanter-reports-cli-{}", crate::secrets::random_hex()?));
        let store = test_accounts(&dir);
        let mut set = tokio::task::JoinSet::new();
        for role in [Role::Admin, Role::Viewer, Role::Admin, Role::Viewer] {
            let store = store.clone();
            set.spawn(async move { store.add("same", role, "2026-10-28").await });
        }
        let mut won = 0;
        while let Some(joined) = set.join_next().await {
            won += usize::from(joined??.is_some());
        }
        assert_eq!(won, 1);
        assert_eq!(store.users().await?.len(), 1);
        let roster = store.roster().await?;
        assert_eq!(roster.admins + roster.viewers, 1);
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
