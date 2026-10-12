//! How the desktop tells one game launched from decanter from another:
//! a desktop entry per game naming its window class, so the task bar shows
//! the game's own title and icon on its window instead of a generic one.
//!
//! The entry and its icon live in the profile's library like the rest of
//! decanter's data (`<library>/decanter/desktop/game.<key>.desktop`,
//! `<library>/decanter/icons/<key>.png`). The desktop only reads entries
//! from its applications folders, so `~/.local/share/applications/decanter/`
//! holds one file symlink to each and nothing else: a symlink to the whole
//! folder was tried and Plasma ignored what was in it. The windows are
//! matched by `StartupWMClass`, never by the entry's file name.

use std::io;
use std::path::{Path, PathBuf};

use decanter_core::{InstalledGame, Storefront};

/// A PC game's [`decanter_core::game_key`], prefixed by where it came from
/// so a GOG product id and an external game's uuid never meet.
pub fn key(storefront: Storefront, game_id: &str) -> String {
    let source = if storefront == Storefront::Gog { "gog" } else { "ext" };
    decanter_core::game_key(&format!("{source}_{game_id}"))
}

/// What one game's desktop entry says.
struct Entry<'entry> {
    key: &'entry str,
    title: &'entry str,
    window_class: &'entry str,
    /// Art URL, or `file://` path, the icon is made from.
    icon: &'entry str,
    /// How the task bar starts the game again; `None` where decanter has
    /// no command line for it.
    exec: Option<String>,
    /// What has to exist for the entry to stay: the game's manifest, or
    /// its dump.
    source: &'entry Path,
}

/// The real folder under the user's applications folder that holds
/// decanter's symlinks.
fn links_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("applications").join("decanter"))
}

fn desktop_dir(library_root: &Path) -> PathBuf {
    library_root.join("decanter").join("desktop")
}

fn desktop_file(library_root: &Path, key: &str) -> PathBuf {
    desktop_dir(library_root).join(format!("game.{key}.desktop"))
}

fn icon_file(library_root: &Path, key: &str) -> PathBuf {
    library_root.join("decanter").join("icons").join(format!("{key}.png"))
}

/// umu derives Proton's `SteamAppId` from `GAMEID`, and Proton's Wine names
/// every X11 window `steam_app_<SteamAppId>`. `umu-default` (every external
/// game umu has no entry for) and an id umu doesn't take apart (which it
/// runs as `0`) are shared by unrelated games, so those get no class here
/// and their windows keep the icon Wine reads from the executable.
fn umu_window_class(game_id: &str) -> Option<String> {
    let app_id = game_id.strip_prefix("umu-")?;
    let distinct =
        !app_id.is_empty() && app_id != "default" && app_id.chars().all(|ch| ch.is_alphanumeric() || ch == '_');
    distinct.then(|| format!("steam_app_{app_id}"))
}

/// Escapes a desktop entry string value.
fn value(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
}

/// One `Exec` argument, quoted the way the desktop entry spec reads it:
/// quoting first, then the string value's own escaping on top.
fn exec_arg(arg: &str) -> String {
    let mut quoted = String::from("\"");
    for ch in arg.chars() {
        match ch {
            '"' | '`' | '$' => {
                quoted.push_str("\\\\");
                quoted.push(ch);
            }
            '\\' => quoted.push_str("\\\\\\\\"),
            '%' => quoted.push_str("%%"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

fn render(entry: &Entry<'_>, icon: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={}\nIcon={}\nExec={}\nNoDisplay=true\nStartupWMClass={}\n\
         X-Decanter-Source={}\nX-Decanter-Icon-Source={}\n",
        value(entry.title),
        value(&icon.to_string_lossy()),
        entry.exec.as_deref().unwrap_or("true"),
        value(entry.window_class),
        value(&entry.source.to_string_lossy()),
        value(entry.icon),
    )
}

/// The `X-Decanter-Source` of an entry decanter wrote, unescaped.
fn source_of(desktop: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(desktop).ok()?;
    let line = text.lines().find_map(|line| line.strip_prefix("X-Decanter-Source="))?;
    let mut unescaped = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            unescaped.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => unescaped.push('\n'),
            Some('t') => unescaped.push('\t'),
            Some('r') => unescaped.push('\r'),
            Some(other) => unescaped.push(other),
            None => {}
        }
    }
    Some(PathBuf::from(unescaped))
}

/// Writes the entry and its icon and links it, returning whether anything
/// changed — the desktop's cache only needs a [`refresh`] then. The icon is
/// only made again when its art changed, so a game launched offline keeps
/// the one it has; an icon that can't be made at all leaves the game without
/// an entry rather than with a blank one.
async fn write(library_root: &Path, entry: &Entry<'_>) -> bool {
    let desktop = desktop_file(library_root, entry.key);
    let icon = icon_file(library_root, entry.key);
    let text = render(entry, &icon);
    let current = tokio::fs::read_to_string(&desktop).await.ok();
    let has_icon = tokio::fs::try_exists(&icon).await.unwrap_or(false);
    let icon_line = format!("\nX-Decanter-Icon-Source={}\n", value(entry.icon));
    let icon_changed = !has_icon || current.as_deref().is_none_or(|current| !current.contains(&icon_line));
    if icon_changed && let Err(err) = crate::art::save_png(entry.icon, &icon).await {
        tracing::warn!(key = entry.key, %err, "could not make the game's task bar icon");
        // Left as it was, to try again next time.
        return if has_icon {
            false
        } else {
            remove(library_root, entry.key)
        };
    }
    let text_changed = current.as_deref() != Some(text.as_str());
    if text_changed && let Err(err) = write_file(&desktop, &text).await {
        tracing::warn!(path = %desktop.display(), %err, "could not write the game's desktop entry");
        return icon_changed;
    }
    let changed = icon_changed || text_changed;
    match link(&desktop, entry.key).await {
        Ok(linked) => changed || linked,
        Err(err) => {
            tracing::warn!(key = entry.key, %err, "could not link the game's desktop entry");
            changed
        }
    }
}

/// Written aside and renamed into place, so the desktop never reads half
/// an entry.
async fn write_file(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let partial = path.with_extension("part");
    tokio::fs::write(&partial, text).await?;
    tokio::fs::rename(&partial, path).await
}

/// Points the game's symlink at `desktop`, returning whether it had to.
async fn link(desktop: &Path, key: &str) -> io::Result<bool> {
    let Some(dir) = links_dir() else {
        return Ok(false);
    };
    let path = dir.join(format!("game.{key}.desktop"));
    if tokio::fs::read_link(&path).await.is_ok_and(|target| target == desktop) {
        return Ok(false);
    }
    tokio::fs::create_dir_all(&dir).await?;
    match tokio::fs::remove_file(&path).await {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        Ok(()) | Err(_) => {}
    }
    tokio::fs::symlink(desktop, &path).await?;
    Ok(true)
}

/// Drops `key`'s entry, icon and symlink, returning whether any was there.
/// The symlink only while it points at this library's entry: another
/// profile's library may hold the same game, and the link may be its.
pub fn remove(library_root: &Path, key: &str) -> bool {
    let desktop = desktop_file(library_root, key);
    let link = links_dir()
        .map(|dir| dir.join(format!("game.{key}.desktop")))
        .filter(|link| std::fs::read_link(link).is_ok_and(|target| target == desktop));
    [Some(desktop), Some(icon_file(library_root, key)), link]
        .into_iter()
        .flatten()
        .fold(false, |removed, path| match std::fs::remove_file(&path) {
            Ok(()) => true,
            Err(err) => {
                if err.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(path = %path.display(), %err, "could not remove a game's task bar entry");
                }
                removed
            }
        })
}

/// After the library at `from` moved to `to`: its entries still name paths
/// under `from`, and the symlinks to them point there. Returns whether
/// anything changed. A symlink to another library's entry for the same game
/// is left alone, as in [`remove`].
pub fn relocate(from: &Path, to: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(desktop_dir(to)) else {
        return false;
    };
    entries.flatten().map(|entry| entry.path()).fold(false, |changed, desktop| {
        let Some(key) = key_of(&desktop) else {
            return changed;
        };
        let rewritten = rewrite(&desktop, from, to)
            .inspect_err(|err| tracing::warn!(path = %desktop.display(), %err, "could not rewrite a moved game's desktop entry"))
            .unwrap_or(false);
        let relinked = relink(&desktop, &desktop_file(from, &key), &key)
            .inspect_err(|err| tracing::warn!(key, %err, "could not re-point a moved game's task bar link"))
            .unwrap_or(false);
        changed | rewritten | relinked
    })
}

fn rewrite(desktop: &Path, from: &Path, to: &Path) -> io::Result<bool> {
    let text = std::fs::read_to_string(desktop)?;
    let rebased = rebase(&text, from, to);
    if rebased == text {
        return Ok(false);
    }
    let partial = desktop.with_extension("part");
    std::fs::write(&partial, rebased)?;
    std::fs::rename(&partial, desktop)?;
    Ok(true)
}

/// Points `key`'s symlink at `desktop` while it points at `old`.
fn relink(desktop: &Path, old: &Path, key: &str) -> io::Result<bool> {
    let Some(link) = links_dir().map(|dir| dir.join(format!("game.{key}.desktop"))) else {
        return Ok(false);
    };
    if !std::fs::read_link(&link).is_ok_and(|target| target == old) {
        return Ok(false);
    }
    std::fs::remove_file(&link)?;
    std::os::unix::fs::symlink(desktop, &link)?;
    Ok(true)
}

/// `text` with the paths under `from` in an entry's values, and the
/// library named in its `Exec`, moved to `to`. Both escapings work a
/// character at a time, so an escaped root is a prefix of an escaped path.
fn rebase(text: &str, from: &Path, to: &Path) -> String {
    let (from, to) = (from.to_string_lossy(), to.to_string_lossy());
    let (from_value, to_value) = (value(&from), value(&to));
    let (from_arg, to_arg) = (exec_arg(&from), exec_arg(&to));
    text.lines()
        .map(|line| {
            if let Some(exec) = line.strip_prefix("Exec=") {
                return format!("Exec={}\n", exec.replace(&from_arg, &to_arg));
            }
            let Some((key, path)) = line.split_once('=') else {
                return format!("{line}\n");
            };
            let (scheme, path) = path
                .strip_prefix("file://")
                .map_or(("", path), |path| ("file://", path));
            path.strip_prefix(from_value.as_str())
                .filter(|rest| rest.starts_with('/'))
                .map_or_else(
                    || format!("{line}\n"),
                    |rest| format!("{key}={scheme}{to_value}{rest}\n"),
                )
        })
        .collect()
}

/// Has the desktop read the applications folders again. Best-effort: a
/// desktop without `kbuildsycoca6` notices new entries on its own, later.
pub async fn refresh() {
    let mut command = tokio::process::Command::new("kbuildsycoca6");
    decanter_core::strip_launcher_env(command.as_std_mut());
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match command.status().await {
        Ok(status) if !status.success() => tracing::warn!(code = status.code(), "kbuildsycoca6 failed"),
        Ok(_) => tracing::debug!("desktop entries refreshed"),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(%err, "could not run kbuildsycoca6"),
    }
}

/// Writes or updates a PC game's entry, before it launches and once it's
/// installed or added. A game whose window can't be told apart from
/// another's (see [`umu_window_class`]) has its entry removed instead.
pub async fn sync_pc(game: &InstalledGame) {
    let Ok(library) = crate::core::library_for_account(&game.account_id) else {
        return;
    };
    let key = key(game.storefront, &game.game_id);
    let gog = game.storefront == Storefront::Gog;
    let umu_id = if gog {
        Some(format!("umu-{}", game.game_id))
    } else {
        game.umu_id.clone()
    };
    let icon = if gog {
        crate::core::db()
            .ok()
            .and_then(|db| db.list_games(&game.account_id).ok())
            .and_then(|games| games.into_iter().find(|cached| cached.product_id == game.game_id))
            .and_then(|cached| cached.icon_url)
    } else {
        game.art.icon.clone()
    };
    let (Some(window_class), Some(icon), Some(source)) = (
        umu_id.as_deref().and_then(umu_window_class),
        icon,
        library.manifest_path(&game.account_id, game.storefront, &game.game_id),
    ) else {
        if remove(library.root(), &key) {
            refresh().await;
        }
        return;
    };
    let exec = gog.then(|| relaunch(&game.account_id, &game.game_id));
    let entry = Entry {
        key: &key,
        title: &game.name,
        window_class: &window_class,
        icon: &icon,
        exec,
        source: &source,
    };
    if write(library.root(), &entry).await {
        refresh().await;
    }
}

/// Writes or updates a PS game's entry before it launches. Its window
/// class is its application id, which the port is told to use.
pub async fn sync_ps(library_root: &Path, game: &decanter_ps::library::Game) {
    let key = game.key();
    let window_class = decanter_core::application_id(&key);
    let icon = format!("file://{}", game.dir.join("sce_sys").join("icon0.png").display());
    let entry = Entry {
        key: &key,
        title: &game.title,
        window_class: &window_class,
        icon: &icon,
        exec: None,
        source: &game.dir,
    };
    if write(library_root, &entry).await {
        refresh().await;
    }
}

/// `decanter launch` for a GOG game, through the `AppImage` when decanter
/// runs as one: the binary it runs from is inside a mount that moves. It
/// names the library, so the game starts from it whichever one is active.
fn relaunch(account_id: &str, product_id: &str) -> String {
    let binary = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("decanter"));
    let binary = binary.to_string_lossy();
    let library = match decanter_core::bootstrap::location() {
        decanter_core::bootstrap::Location::Library(root) => Some(root.to_string_lossy().into_owned()),
        decanter_core::bootstrap::Location::Legacy(_) | decanter_core::bootstrap::Location::Unconfigured => None,
    };
    std::iter::once(binary.as_ref())
        .chain(library.iter().flat_map(|root| ["--library", root.as_str()]))
        .chain(["launch", "--account-id", account_id, "--product-id", product_id])
        .map(exec_arg)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Removes a PC game's entry once it's uninstalled or removed.
pub async fn remove_pc(account_id: &str, storefront: Storefront, game_id: &str) {
    let Ok(library) = crate::core::library_for_account(account_id) else {
        return;
    };
    if remove(library.root(), &key(storefront, game_id)) {
        refresh().await;
    }
}

/// At startup: drops every symlink that points nowhere, or anywhere but a
/// decanter library's entries, and every entry whose game is gone. A known
/// library whose disk is away keeps its symlinks for when it's back.
pub async fn sweep() {
    match tokio::task::spawn_blocking(remove_stale).await {
        Ok(true) => refresh().await,
        Ok(false) => {}
        Err(err) => tracing::warn!(%err, "task bar entry sweep failed"),
    }
}

/// [`sweep`]'s blocking file I/O and database reads, returning whether
/// anything went.
fn remove_stale() -> bool {
    let mut roots: Vec<PathBuf> = crate::core::db()
        .and_then(|db| db.list_profiles().map_err(|err| err.to_string()))
        .unwrap_or_default()
        .iter()
        .filter_map(|profile| crate::core::library_for_profile(&profile.id).ok())
        .map(|library| library.root().to_owned())
        .chain(decanter_core::bootstrap::active_root())
        .chain(decanter_core::bootstrap::known_roots())
        .collect();
    roots.sort();
    roots.dedup();
    let mut changed = false;
    for root in &roots {
        let Ok(entries) = std::fs::read_dir(desktop_dir(root)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let stale = source_of(&path).is_none_or(|source| !source.exists());
            if let Some(key) = stale.then(|| key_of(&path)).flatten() {
                changed |= remove(root, &key);
            }
        }
    }
    let Some(entries) = links_dir().and_then(|dir| std::fs::read_dir(dir).ok()) else {
        return changed;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let kept = std::fs::read_link(&path).is_ok_and(|target| {
            key_of(&target).is_some()
                && path.file_name() == target.file_name()
                && roots.iter().any(|root| {
                    target.parent() == Some(desktop_dir(root).as_path()) && (target.is_file() || !root.is_dir())
                })
        });
        if !kept {
            match std::fs::remove_file(&path) {
                Ok(()) => changed = true,
                Err(err) => tracing::warn!(path = %path.display(), %err, "could not remove a stale task bar link"),
            }
        }
    }
    changed
}

/// The key in a `game.<key>.desktop` file name.
fn key_of(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    name.strip_prefix("game.")?.strip_suffix(".desktop").map(str::to_owned)
}
