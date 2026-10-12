//! AnyPS5: builds the relinker and system libraries, converts a PS5 dump
//! into the runtime layout from `anyps5/docs/user/USAGE.md` and runs it.
//!
//! The conversion is shared by every profile: `<data>/anyps5/<TITLE_ID>/`
//! holds `app.elf`, `libs/` (a link to the built libraries, so a rebuild
//! reaches every game), `app0/` (links to the dump's files plus the
//! relinker's converted modules) and the game's `anyps5-input.ini`.
//!
//! Saves are not: AnyPS5 resolves every guest path, `/_sd` included,
//! against its working directory (`core/libs/prx/libc/src/General.cpp`,
//! `WorkingDirectory`), so each profile runs the shared `app.elf` from a
//! directory of its own that links `app0/` and keeps its own `_sd/`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::library::Game;
use crate::paths::Root;
use crate::port::Port;
use crate::toolchain::Toolchain;

/// Module folders the relinker reads beside the executable and rewrites under `app0/`.
const MODULE_DIRS: [&str; 3] = ["sce_module", "sce_modules", "prx"];
const ELF_MAGIC: &[u8] = b"\x7fELF";

const BUILD_SCRIPT: &str = "set -e; git submodule update --init --recursive; \
    cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_COMPILER=gcc \
    -DCMAKE_CXX_COMPILER=g++ \"-DCMAKE_EXE_LINKER_FLAGS=${ANYPS5_EXE_LDFLAGS:-}\"; \
    cmake --build build --parallel; cmake --build build --target libs --parallel";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eboot {
    /// A plain ELF, what the relinker takes.
    Clean,
    /// Still a signed SELF; it has to be decrypted on the console first.
    Encrypted,
    Missing,
}

/// Reads four bytes of the dump: off the UI thread.
pub fn eboot_state(game: &Game) -> Eboot {
    let mut magic = [0u8; 4];
    match fs::File::open(game.dir.join("eboot.bin")).and_then(|mut file| io::Read::read_exact(&mut file, &mut magic)) {
        Ok(()) if magic == ELF_MAGIC => Eboot::Clean,
        Ok(()) => Eboot::Encrypted,
        Err(_) => Eboot::Missing,
    }
}

/// An AnyPS5 checkout. As with bbport, what one toolchain built only runs
/// inside that toolchain: the libraries link nix's Vulkan loader.
pub struct AnyPs5 {
    root: Root,
    checkout: PathBuf,
}

impl AnyPs5 {
    pub fn new(root: Root) -> Self {
        Self {
            checkout: Port::AnyPs5.checkout(&root),
            root,
        }
    }

    pub fn checkout(&self) -> &Path {
        &self.checkout
    }

    fn relinker(&self) -> PathBuf {
        self.checkout.join("build/core/relinker/relinker")
    }

    fn libs_dir(&self) -> PathBuf {
        self.checkout.join("build/core/libs/libs")
    }

    fn build_marker(&self) -> PathBuf {
        self.checkout.join("build/decanter-toolchain")
    }

    pub async fn build_command(&self, toolchain: &Toolchain) -> io::Result<Command> {
        toolchain.command(&self.root, Port::AnyPs5, BUILD_SCRIPT).await
    }

    /// After a build that succeeded.
    pub async fn record_build(&self, toolchain: &Toolchain) -> io::Result<()> {
        tokio::fs::write(self.build_marker(), toolchain.build_stamp(&self.checkout).await).await
    }

    pub async fn is_built_with(&self, toolchain: &Toolchain) -> bool {
        let relinker = tokio::fs::metadata(self.relinker())
            .await
            .is_ok_and(|meta| meta.is_file());
        let marker = tokio::fs::read_to_string(self.build_marker()).await;
        let stamp = toolchain.build_stamp(&self.checkout).await;
        relinker && marker.is_ok_and(|built| built == stamp)
    }

    pub fn output_dir(root: &Root, title_id: &str) -> PathBuf {
        root.anyps5().join(title_id)
    }

    fn executable(root: &Root, title_id: &str) -> PathBuf {
        Self::output_dir(root, title_id).join("app.elf")
    }

    pub fn binds_path(root: &Root, title_id: &str) -> PathBuf {
        Self::output_dir(root, title_id).join("anyps5-input.ini")
    }

    pub async fn is_converted(&self, title_id: &str) -> bool {
        tokio::fs::metadata(Self::executable(&self.root, title_id))
            .await
            .is_ok_and(|meta| meta.is_file())
    }

    /// Lays out `libs/` and `app0/` before the relinker writes `app.elf`
    /// and the modules. File I/O over the dump's top level: off the UI thread.
    pub fn prepare_output(&self, game: &Game) -> io::Result<()> {
        let out = Self::output_dir(&self.root, &game.title_id);
        let app0 = out.join("app0");
        fs::create_dir_all(&app0)?;
        replace_symlink(&self.libs_dir(), &out.join("libs"))?;
        for entry in fs::read_dir(&game.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if MODULE_DIRS.iter().any(|dir| name == *dir) || name == "eboot.bin" {
                continue;
            }
            replace_symlink(&entry.path(), &app0.join(&name))?;
        }
        Ok(())
    }

    /// Paths reach the script through the environment, not its text, so
    /// none of them is ever parsed by the shell. The relinker takes no
    /// `--`; both paths are absolute, so neither can read as a flag.
    pub async fn convert_command(&self, toolchain: &Toolchain, game: &Game) -> io::Result<Command> {
        let script = r#"exec "$DECANTER_RELINKER" "$DECANTER_EBOOT" "$DECANTER_APP""#;
        let mut command = match toolchain.command(&self.root, Port::AnyPs5, script).await {
            Ok(command) => command,
            Err(err) => return Err(err),
        };
        command
            .env("DECANTER_RELINKER", self.relinker())
            .env("DECANTER_EBOOT", game.dir.join("eboot.bin"))
            .env("DECANTER_APP", Self::executable(&self.root, &game.title_id))
            .current_dir(Self::output_dir(&self.root, &game.title_id));
        Ok(command)
    }

    /// Runs the shared conversion from `run_dir`, the profile's own
    /// directory for this game; [`prepare_run_dir`] must have laid it out.
    /// SDL 2 reads its window class hints from the environment, `app.elf`
    /// for every title otherwise, and the task bar matches the game's
    /// desktop entry against them.
    pub async fn run_command(&self, toolchain: &Toolchain, game: &Game, run_dir: &Path) -> io::Result<Command> {
        let mut command = match toolchain
            .command(&self.root, Port::AnyPs5, r#"exec "$DECANTER_APP""#)
            .await
        {
            Ok(command) => command,
            Err(err) => return Err(err),
        };
        let class = decanter_core::application_id(&game.key());
        command
            .env("DECANTER_APP", Self::executable(&self.root, &game.title_id))
            .env("SDL_VIDEO_X11_WMCLASS", &class)
            .env("SDL_VIDEO_WAYLAND_WMCLASS", &class)
            .current_dir(run_dir);
        Ok(command)
    }
}

/// `run_dir` gets `app0/` linked to the shared conversion; `_sd/` is left
/// for AnyPS5 to create there. The conversion's own `libs` link is pointed
/// at this checkout's build again too: the PS folder moves with the
/// library, and an absolute link from before a move points nowhere. File
/// I/O: off the UI thread.
pub fn prepare_run_dir(anyps5: &AnyPs5, title_id: &str, run_dir: &Path) -> io::Result<()> {
    let out = AnyPs5::output_dir(&anyps5.root, title_id);
    replace_symlink(&anyps5.libs_dir(), &out.join("libs"))?;
    fs::create_dir_all(run_dir)?;
    replace_symlink(&out.join("app0"), &run_dir.join("app0"))
}

fn replace_symlink(target: &Path, link: &Path) -> io::Result<()> {
    if link.symlink_metadata().is_ok() {
        fs::remove_file(link)?;
    }
    std::os::unix::fs::symlink(target, link)
}
