//! What a Windows build downloads, each piece pinned here by SHA-256 and checked before anything in
//! it is read: llvm-mingw, CMake and Ninja from their upstream releases, and psport's toolchain package
//! (`toolchain-win32-N`, psport's `docs/en/WINDOWS_TOOLCHAIN.md`). The package's `manifest.json`
//! must name the same three tools by the same hashes ([`super::toolchain`]).

use crate::archive::{Limits, Names};

/// psport's toolchain package this decanter builds with: the one place a new `toolchain-win32-N` is
/// pinned, from its release's `.zip.sha256` and the asset's size (the procedure is in
/// `docs/windows-support.md`). A `sha256` that isn't one stops every build with
/// [`super::Error::NotPinned`] before anything is downloaded.
pub const TOOLCHAIN: Toolchain = Toolchain {
    number: 1,
    sha256: "4f9ee32b4906d19875e6ecda154743ac678b4b2795c2b177faaed197cc6a87d0",
    size: 317_092_630,
};

pub struct Toolchain {
    pub number: u32,
    pub sha256: &'static str,
    /// Bytes of the zip, as its release lists it.
    pub size: u64,
}

/// One zip to download, check and extract. Every field is checked against what lands, so a value
/// that drifted upstream stops the install instead of passing.
#[derive(Debug, Clone)]
pub struct Archive {
    /// What the log and the UI call it.
    pub name: String,
    pub url: String,
    pub sha256: String,
    /// Bytes, checked as the download arrives when known.
    pub size: Option<u64>,
    /// The one folder its files sit in, or empty for an archive with its files at the top.
    pub folder: String,
    /// The folder it is installed to inside the toolchain cache, and the name of its record.
    pub key: String,
    pub names: Names,
    pub limits: Limits,
    /// `/`-separated paths below the folder that must be files once it is extracted.
    pub required: Vec<String>,
}

/// The folder llvm-mingw's zip unpacks into, which the toolchain file takes as `PS_LLVM_MINGW`.
pub const LLVM_MINGW_FOLDER: &str = "llvm-mingw-20261020-ucrt-x86_64";

/// The three tools the toolchain package's manifest names, in its order.
#[must_use]
pub fn tools() -> [Archive; 3] {
    [
        // [llvm-mingw 20261020](https://github.com/mstorsjo/llvm-mingw/releases/tag/20261020) (LLVM
        // 23.1.4, UCRT): the release asset's digest in GitHub's API, as the project publishes no checksum
        // file. Measured: 9,299 entries, 749,515,686 bytes unpacked, the largest file 77,677,056 bytes
        // (`bin/libLLVM-23.dll`).
        Archive {
            name: "llvm-mingw".to_owned(),
            url: format!("https://github.com/mstorsjo/llvm-mingw/releases/download/20261020/{LLVM_MINGW_FOLDER}.zip"),
            sha256: "fd5462b7f5d93b2ff981fcd3f9c7e427e47f3203c64ea91c27bcc444eebd74cd".to_owned(),
            size: Some(190_612_575),
            folder: LLVM_MINGW_FOLDER.to_owned(),
            key: LLVM_MINGW_FOLDER.to_owned(),
            names: Names::Plain,
            limits: Limits {
                entries: 12_000,
                unpacked_bytes: 1 << 30,
                file_bytes: 128 << 20,
            },
            required: [
                "clang.exe",
                "clang++.exe",
                "ld.lld.exe",
                "llvm-ar.exe",
                "llvm-ranlib.exe",
                "llvm-windres.exe",
                "llvm-dlltool.exe",
            ]
            .into_iter()
            .map(|tool| format!("bin/{tool}"))
            .collect(),
        },
        // [CMake 4.4.4](https://cmake.org/download/): `cmake-4.4.4-SHA-256.txt`, signed by Kitware. Measured:
        // 8,979 entries, 143,310,580 bytes unpacked, the largest file 33,879,632 bytes; its documentation
        // has names with spaces (`Unix Makefiles.html`).
        Archive {
            name: "CMake".to_owned(),
            url: "https://github.com/Kitware/CMake/releases/download/v4.4.4/cmake-4.4.4-windows-x86_64.zip".to_owned(),
            sha256: "bace36e94b31c68ab6fa295f26dfa11219e0701cf7c94b0284a7d1cb13dac536".to_owned(),
            size: Some(54_400_562),
            folder: "cmake-4.4.4-windows-x86_64".to_owned(),
            key: "cmake-4.4.4-windows-x86_64".to_owned(),
            names: Names::Toolchain,
            limits: Limits {
                entries: 12_000,
                unpacked_bytes: 256 << 20,
                file_bytes: 64 << 20,
            },
            required: vec!["bin/cmake.exe".to_owned()],
        },
        // [Ninja 1.13.2](https://github.com/ninja-build/ninja/releases/tag/v1.13.2): the release asset's
        // digest in GitHub's API. One file, `ninja.exe`, 603,648 bytes, at the zip's top.
        Archive {
            name: "Ninja".to_owned(),
            url: "https://github.com/ninja-build/ninja/releases/download/v1.13.2/ninja-win.zip".to_owned(),
            sha256: "07fc8261b42b20e71d1720b39068c2e14ffcee6396b76fb7a795fb460b78dc65".to_owned(),
            size: Some(291_570),
            folder: String::new(),
            key: "ninja-1.13.2".to_owned(),
            names: Names::Plain,
            limits: Limits {
                entries: 4,
                unpacked_bytes: 4 << 20,
                file_bytes: 4 << 20,
            },
            required: vec!["ninja.exe".to_owned()],
        },
    ]
}

/// The manifest's names for [`tools`], in the same order.
pub const TOOL_NAMES: [&str; 3] = ["llvm-mingw", "cmake", "ninja"];

/// [`TOOLCHAIN`]'s package, or `None` when its SHA-256 is no SHA-256. Toolchain 1 as published:
/// 54,558 entries (51,863 files), 1,070,375,859 bytes unpacked, the largest file 33,122,816 bytes,
/// the longest path 159 characters, and `@` in 15 locale folders (`sr@latin`);
/// the caps leave room for its folders and a later package, not for an archive of another kind.
#[must_use]
pub fn package() -> Option<Archive> {
    let pin = &TOOLCHAIN;
    if !is_sha256(pin.sha256) {
        return None;
    }
    let folder = package_folder(pin.number);
    Some(Archive {
        name: format!("toolchain-win32-{}", pin.number),
        url: format!(
            "https://github.com/yerasuu/psport/releases/download/toolchain-win32-{number}/{folder}.zip",
            number = pin.number
        ),
        sha256: pin.sha256.to_owned(),
        size: Some(pin.size),
        key: folder.clone(),
        folder,
        names: Names::Toolchain,
        limits: Limits {
            entries: 80_000,
            unpacked_bytes: 3 << 29,
            file_bytes: 256 << 20,
        },
        required: vec![
            "manifest.json".to_owned(),
            "psport-toolchain.cmake".to_owned(),
            "clang64/bin/pkgconf.exe".to_owned(),
            "clang64/bin/glslang.exe".to_owned(),
        ],
    })
}

#[must_use]
pub fn package_folder(number: u32) -> String {
    format!("psport-toolchain-win32-{number}-x86_64")
}

#[must_use]
pub fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}
