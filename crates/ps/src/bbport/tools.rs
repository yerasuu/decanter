//! What bbport's own launcher managed: mods, external patches and the FSR
//! 4.1.1 build. The discovery rules stay bbport's: its Python helpers are
//! imported read-only (`scripts/mods.py`, `scripts/patches.py`,
//! `tools/fsr4cap/dll_info.py`) and run inside the toolchain like the rest
//! of the port; the selections go to the files `run.sh` reads
//! (`mods.json`, `patches.json` in the data directory).

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::paths::Root;
use crate::port::Port;
use crate::toolchain::Toolchain;

/// Imports bbport's helpers and prints JSON for one request: `mods <dir>`,
/// `patches <dir>`, `dll <upscaler> [loader]`. Everything arrives through
/// the environment, so no path is ever parsed by a shell.
const HELPER: &str = r#"
import json, os, sys
root = os.environ['DECANTER_ROOT']
sys.path[:0] = [root + '/scripts', root + '/tools/fsr4cap']
request = os.environ['DECANTER_REQUEST']
args = [os.environ[key] for key in ('DECANTER_ARG0', 'DECANTER_ARG1') if os.environ.get(key)]
if request == 'mods':
    import mods
    print(json.dumps(mods.discover(args[0])))
elif request == 'patches':
    import contextlib, io, patches
    with contextlib.redirect_stderr(io.StringIO()):
        found = patches.external_patches(args[0])
    print(json.dumps([{'key': key, 'name': meta.get('Name') or key, 'author': meta.get('Author') or '',
                       'note': (meta.get('Note') or '').replace('\\n', '\n'),
                       'default': (meta.get('isEnabled') or 'false').lower() == 'true'}
                      for key, _path, meta in found]))
elif request == 'dll':
    import contextlib, io, dll_info
    with contextlib.redirect_stdout(io.StringIO()):
        status, problem, details = dll_info.inspect(args[0], args[1] if len(args) > 1 else None)
    details = {k: (str(v) if k == 'loader' and v else v) for k, v in details.items()}
    print(json.dumps({'status': status, 'problem': problem, 'details': details,
                      'unsupported': dll_info.UNSUPPORTED, 'no_loader': dll_info.NO_LOADER}))
"#;

async fn helper(
    root: &Root,
    toolchain: &Toolchain,
    request: &str,
    args: &[&Path],
) -> Result<serde_json::Value, String> {
    let checkout = Port::Bbport.checkout(root);
    let mut command = match toolchain
        .command(root, Port::Bbport, r#"exec python3 -c "$DECANTER_HELPER""#)
        .await
    {
        Ok(command) => command,
        Err(err) => return Err(format!("python3: {err}")),
    };
    command
        .env("DECANTER_HELPER", HELPER)
        .env("DECANTER_ROOT", &checkout)
        .env("DECANTER_REQUEST", request);
    for (key, arg) in ["DECANTER_ARG0", "DECANTER_ARG1"].into_iter().zip(args) {
        command.env(key, arg);
    }
    let output = match command.output().await {
        Ok(output) => output,
        Err(err) => return Err(format!("python3: {err}")),
    };
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .lines()
            .last()
            .unwrap_or("error")
            .to_owned());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

/// Mods bbport would load from `dir` (folders with a layout `mods.py` accepts).
pub async fn discover_mods(root: &Root, toolchain: &Toolchain, dir: &Path) -> Result<Vec<String>, String> {
    match helper(root, toolchain, "mods", &[dir]).await {
        Ok(value) => serde_json::from_value(value).map_err(|err| err.to_string()),
        Err(err) => Err(err),
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExternalPatch {
    /// `<file name>/<patch name>`, the key of `patches.json`.
    pub key: String,
    pub name: String,
    pub author: String,
    pub note: String,
    /// The file's own `isEnabled`.
    pub default: bool,
}

/// shadPS4-format patches for 1.09 in `dir`, as bbport's `external_patches` finds them.
pub async fn discover_patches(root: &Root, toolchain: &Toolchain, dir: &Path) -> Result<Vec<ExternalPatch>, String> {
    match helper(root, toolchain, "patches", &[dir]).await {
        Ok(value) => serde_json::from_value(value).map_err(|err| err.to_string()),
        Err(err) => Err(err),
    }
}

/// `mods.json`: load order and the mods switched off.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ModProfile {
    #[serde(default)]
    pub order: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
}

impl ModProfile {
    pub fn path(root: &Root) -> PathBuf {
        root.bbport_data().join("mods.json")
    }

    /// File I/O, like [`ModProfile::save`]: off the UI thread.
    pub fn load(path: &Path) -> ModProfile {
        std::fs::read(path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    /// The launcher's order: saved names that still exist, then new ones alphabetically.
    pub fn ordered(&self, available: &[String]) -> Vec<String> {
        let mut seen = BTreeSet::new();
        self.order
            .iter()
            .chain(available)
            .filter(|name| available.contains(name) && seen.insert(name.as_str()))
            .cloned()
            .collect()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)? + "\n";
        crate::files::write_atomic(path, text.as_bytes())
    }
}

/// `patches.json`: only the switches that differ from each file's `isEnabled`.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PatchProfile {
    #[serde(default)]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
}

impl PatchProfile {
    pub fn path(root: &Root) -> PathBuf {
        root.bbport_data().join("patches.json")
    }

    /// File I/O, like [`PatchProfile::save`]: off the UI thread.
    pub fn load(path: &Path) -> PatchProfile {
        std::fs::read(path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    pub fn is_active(&self, patch: &ExternalPatch) -> bool {
        self.enabled.contains(&patch.key) || (patch.default && !self.disabled.contains(&patch.key))
    }

    /// Records a switch; returning to the file's default removes the override.
    pub fn set(&mut self, patch: &ExternalPatch, active: bool) {
        self.enabled.retain(|k| k != &patch.key);
        self.disabled.retain(|k| k != &patch.key);
        if active != patch.default {
            if active { &mut self.enabled } else { &mut self.disabled }.push(patch.key.clone());
        }
        self.enabled.sort();
        self.disabled.sort();
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)? + "\n";
        crate::files::write_atomic(path, text.as_bytes())
    }
}

/// The first FSR 4.1.1 asset missing or damaged, as `<model>/<file>`.
///
/// What `fsr411_problem` (bbport `launcher/bbport_assets.py`) checks: every pass of the model
/// the output size and preset need, plus its initializer. File I/O: off the UI thread.
pub fn fsr411_problem(dir: &Path, output: &str, preset: &str) -> Option<String> {
    let (w, h) = output
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))?;
    let model = format!(
        "{}{}",
        if w > 1920 || h > 1080 { "t2160" } else { "t1080" },
        if preset == "4" { "_m1" } else { "_m0" }
    );
    let passes = ["spd", "prepass", "pass0_post"]
        .into_iter()
        .map(str::to_owned)
        .chain((1..=12).flat_map(|i| [format!("pass{i}"), format!("pass{i}_post")]))
        .chain(["postpass".to_owned(), "rcas".to_owned()]);
    for name in passes {
        let size = std::fs::metadata(dir.join(&model).join(format!("{name}.spv")))
            .map(|m| m.len())
            .unwrap_or(0);
        if size < 20 || !size.is_multiple_of(4) {
            return Some(format!("{model}/{name}.spv"));
        }
    }
    let size = std::fs::metadata(dir.join(&model).join("initializer.bin"))
        .map(|m| m.len())
        .unwrap_or(0);
    (size != 131_072).then(|| format!("{model}/initializer.bin"))
}

pub enum DllCheck {
    Ready {
        loader: Option<PathBuf>,
    },
    /// Not a DLL bbport can replay.
    Unsupported(Unsupported),
    /// The DLL needs the FidelityFX loader and none was found next to it.
    NeedsLoader,
    /// The loader given is not one.
    BadLoader(BadLoader),
}

pub enum Unsupported {
    /// AMD's official FSR 4.0.x.
    Fsr40 {
        version: String,
    },
    NotFsr4,
    /// Not AMD's official FSR 4.1.x.
    Other {
        version: String,
    },
}

pub enum BadLoader {
    /// The upscaler DLL itself.
    IsUpscaler,
    WrongVersion {
        loader: PathBuf,
        version: String,
    },
}

/// `dll_info.inspect` on the chosen DLLs, with bbport's launcher's explanations.
pub async fn check_dll(
    root: &Root,
    toolchain: &Toolchain,
    upscaler: &Path,
    loader: Option<&Path>,
) -> Result<DllCheck, String> {
    let mut args = vec![upscaler];
    args.extend(loader);
    let answer = match helper(root, toolchain, "dll", &args).await {
        Ok(answer) => answer,
        Err(err) => return Err(err),
    };
    let status = answer["status"].as_i64().unwrap_or(-1);
    let details = &answer["details"];
    let kind = details["kind"].as_str().unwrap_or_default();
    let version = details["version"].as_str().unwrap_or("?").to_owned();
    if status == answer["unsupported"].as_i64().unwrap_or(2) {
        return Ok(DllCheck::Unsupported(match kind {
            "fsr4_0" => Unsupported::Fsr40 { version },
            "not_fsr4" => Unsupported::NotFsr4,
            _ => Unsupported::Other { version },
        }));
    }
    if status == answer["no_loader"].as_i64().unwrap_or(7) {
        return Ok(match (kind, loader) {
            ("loader_is_upscaler", _) => DllCheck::BadLoader(BadLoader::IsUpscaler),
            (_, Some(loader)) => DllCheck::BadLoader(BadLoader::WrongVersion {
                loader: loader.to_owned(),
                version: details["loader_version"].as_str().unwrap_or("?").to_owned(),
            }),
            _ => DllCheck::NeedsLoader,
        });
    }
    Ok(DllCheck::Ready {
        loader: details["loader"].as_str().map(PathBuf::from),
    })
}

/// `tools/fsr4cap/build_assets.sh` writing into decanter's FSR 4.1.1
/// directory, which `launch_command` passes as `BB_FSR411_DIR`. The work
/// folder must be under `$HOME` for Steam's runtime, which decanter's data
/// directory is.
pub async fn fsr411_build_command(
    root: &Root,
    toolchain: &Toolchain,
    upscaler: &Path,
    loader: Option<&Path>,
) -> io::Result<Command> {
    let script =
        r#"exec bash tools/fsr4cap/build_assets.sh "$DECANTER_UPSCALER" ${DECANTER_LOADER:+"$DECANTER_LOADER"}"#;
    let mut command = match toolchain.command(root, Port::Bbport, script).await {
        Ok(command) => command,
        Err(err) => return Err(err),
    };
    command
        .env("DECANTER_UPSCALER", upscaler)
        .env("BB_FSR411_OUT", root.fsr411())
        .env("BB_FSR4CAP_WORK", root.join("fsr4cap"))
        .env("BB_FSR4CAP_CLEAN", "1");
    if let Some(loader) = loader {
        command.env("DECANTER_LOADER", loader);
    }
    Ok(command)
}

pub enum Fsr411Progress {
    Preparing,
    /// Passes recorded under Proton so far, of the total.
    Recording {
        done: String,
        total: String,
    },
    Translating,
}

/// Progress from `build_assets.sh` output, as bbport's launcher read it.
pub fn fsr411_progress(job: &crate::process::Job) -> Fsr411Progress {
    for line in job.lines.iter().rev() {
        if let Some((done, total)) = line
            .trim_start()
            .strip_prefix('[')
            .and_then(|rest| rest.split_once(']'))
            .and_then(|(step, _)| step.trim().split_once('/'))
        {
            return Fsr411Progress::Recording {
                done: done.to_owned(),
                total: total.to_owned(),
            };
        }
        if line.starts_with("Translating") {
            return Fsr411Progress::Translating;
        }
    }
    Fsr411Progress::Preparing
}

/// `build_assets.sh` exit statuses, as bbport's launcher explained them.
pub enum Fsr411Failure {
    BadDll,
    NoProton,
    MissingTools,
    NoProtonRan,
    Mismatch,
    NoLoader,
    NixosNeedsRecorder,
    Failed(i32),
    Stopped,
}

pub const fn fsr411_failure(code: Option<i32>) -> Fsr411Failure {
    match code {
        Some(2) => Fsr411Failure::BadDll,
        Some(3) => Fsr411Failure::NoProton,
        Some(4) => Fsr411Failure::MissingTools,
        Some(5) => Fsr411Failure::NoProtonRan,
        Some(6) => Fsr411Failure::Mismatch,
        Some(7) => Fsr411Failure::NoLoader,
        Some(8) => Fsr411Failure::NixosNeedsRecorder,
        Some(code) => Fsr411Failure::Failed(code),
        None => Fsr411Failure::Stopped,
    }
}
