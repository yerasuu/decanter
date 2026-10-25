//! The closed shape of a report.
//!
//! Every field a public report may carry, and nothing else: each struct refuses unknown fields,
//! so a field this file does not name never reaches the server's pages. What each field may hold
//! is in [`crate::validate`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Bumped when a field changes meaning, so the server can tell old clients apart. 2: `failure.message`
/// went from free text to a template id with typed slots ([`Message`]).
pub const SCHEMA: u32 = 2;

/// One report, shaped by its product (decision #991). Serialized untagged because each shape
/// carries its own `product` field; [`crate::validate`] reads that field first to pick the shape.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Report {
    Decanter(DecanterReport),
    Psport(Box<PsportReport>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Product {
    Decanter,
    Psport,
}

/// Something decanter itself failed to do.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecanterReport {
    pub schema: u32,
    pub product: Product,
    /// `YYYY-MM-DD`, UTC: the day only, never the time.
    pub date: String,
    pub decanter: Build,
    pub system: System,
    pub action: Action,
    pub failure: Failure,
}

/// Something a game did under psport, with the game and how it was run.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PsportReport {
    pub schema: u32,
    pub product: Product,
    pub date: String,
    pub decanter: Build,
    pub psport: Build,
    pub system: System,
    pub game: Game,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<Setting, String>,
    pub failure: Failure,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls: Option<Calls>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault: Option<Fault>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perf: Option<Perf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct System {
    pub os: Os,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<Gpu>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Linux,
    Windows,
    Macos,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gpu {
    pub vendor: GpuVendor,
    /// Must start with the vendor's own brand, which keeps a monitor or gamepad name out.
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Amd,
    Nvidia,
    Intel,
    Apple,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Install,
    Launch,
    Sync,
    Settings,
    Build,
    Update,
    Other,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub kind: FailureKind,
    /// Absent when the line matches no template of the closed vocabulary: the kind is all that is said then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    pub count: u32,
}

/// A failure line as a template of [`crate::vocab`] and the values of its typed slots, in the
/// template's order. The text of the line itself never travels.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub template: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<Slot>,
    /// What decanter's own lines append after a colon (`: database is locked`), as a template of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Cause>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cause {
    pub template: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<Slot>,
}

/// The only kinds of value a message carries; each is checked against its own shape and range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Slot {
    Number(u32),
    /// Relative to its image, never an address: at most 32 bits.
    Offset(u32),
    /// An SCE or errno code, signed 32 or unsigned 32 bits.
    Code(i64),
    Call(String),
    Version(String),
    /// The file name of an executable image, without folders.
    Image(String),
    Vuid(String),
    Const(String),
    /// One of the port's log classes (`Lib.Font`).
    Class(String),
    /// One of the alternatives the template lists.
    Choice(String),
}

/// The kinds of `failures.jsonl` (`decanter_ps::failures::Kind`): Missing is what the UI lists as
/// "Falta", Crash as "Cierre".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FailureKind {
    Crash,
    Stop,
    Error,
    Exit,
    Missing,
}

/// psport's game identity: what the dump's `param.sfo` says, never where the dump lives.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Game {
    pub title_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Whether psport applied a game profile of its own (`gpu/games/<game>/`) to the run.
    #[serde(default)]
    pub profile: bool,
}

/// The run settings that change how a game runs, as decanter passes them to psport (`PS_*`).
/// Settings that hold paths, servers or account names (`PS_SHADNET_NPID`) have no key here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Setting {
    AspectRatio,
    FpsMode,
    FpsLimit,
    FrameStats,
    Fullscreen,
    GpuProfile,
    Hdr,
    Language,
    MenuLanguage,
    ModsEnabled,
    Online,
    PcModel,
    Permissive,
    Prebuilt,
    PresentMode,
    SkipGameCheck,
    VkValidation,
}

/// psport's call report (`Calls: summary`), each list most calls first.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Calls {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unimplemented: Vec<Call>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stubs: Vec<Call>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<Call>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status: Vec<Call>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub name: String,
    pub count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<CallCode>,
}

/// Same values as `decanter_ps::failures::CallError`; the aliases read that type's own spelling.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallCode {
    #[serde(alias = "Sce")]
    Sce(u32),
    #[serde(alias = "Errno")]
    Errno(i32),
}

/// Where a run crashed. Offsets are relative to their image, never absolute host addresses, so
/// they hold nothing about the machine's address layout.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fault {
    pub side: FaultSide,
    pub signal: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<Frame>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FaultSide {
    Guest,
    Host,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    /// The image's file name; `None` is the game's own executable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

/// psport's `Frame stats:` lines over the run.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Perf {
    pub samples: u32,
    pub fps_mean: f64,
    pub fps_min: f64,
    pub worst_frame_ms: f64,
}
