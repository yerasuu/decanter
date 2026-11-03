//! Registering a Windows service in a prefix.

use std::env;

use anyhow::{Context as _, bail};

use crate::journal::write_atomic;
use crate::{Launch, Prefix, Proton};

/// Where a prefix's own Windows-side files of decanter's live, under the
/// prefix's `drive_c` and so inside the library with the rest of the prefix.
const PROGRAM_DATA_DIR: [&str; 2] = ["ProgramData", "decanter"];

/// A service decanter registers in a prefix: the binary it ships and the
/// name the service control manager knows it by.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Service<'a> {
    /// The executable's bytes, written into the prefix as `<name>.exe`.
    pub binary: &'a [u8],
    /// Letters and digits only: it becomes a file name and a command argument.
    pub name: &'a str,
}

impl<'a> Service<'a> {
    #[inline]
    #[must_use]
    pub const fn new(name: &'a str, binary: &'a [u8]) -> Self {
        Self { binary, name }
    }
}

impl Prefix {
    /// Registers `service` in the prefix as a demand-start service, once:
    /// a marker beside the prefix's own records says it is done, so later
    /// calls return `false` without starting Wine.
    ///
    /// The binary goes to `C:\ProgramData\decanter\<name>.exe` inside the
    /// prefix and `sc create` runs under `proton` on the bare name `sc`, so
    /// Wine resolves its own. No access control is set beyond what
    /// `sc create` gives: Windows' default security descriptor lets only
    /// SYSTEM and Administrators start or stop the service and gives other
    /// users read access, and Wine runs every prefix as its one user.
    ///
    /// Returns whether it registered the service now.
    ///
    /// # Errors
    ///
    /// Returns an error if `service.name` isn't plain letters and digits,
    /// the binary can't be written, or `sc create` can't run or exits with
    /// a non-zero status; nothing is marked done then, so the next call
    /// tries again.
    #[inline]
    pub async fn ensure_service(&self, proton: &Proton, service: Service<'_>) -> anyhow::Result<bool> {
        if service.name.is_empty() || !service.name.chars().all(|c| c.is_ascii_alphanumeric()) {
            bail!("unusable service name {:?}", service.name);
        }
        let marker = self.path.join(format!("decanter-service-{}", service.name));
        if marker.is_file() {
            return Ok(false);
        }

        let file_name = format!("{}.exe", service.name);
        let dir = PROGRAM_DATA_DIR.iter().fold(self.drive_c(), |dir, part| dir.join(part));
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
        let binary = dir.join(&file_name);
        write_atomic(&binary, service.binary)
            .await
            .with_context(|| format!("writing {}", binary.display()))?;

        tracing::info!(prefix = %self.path.display(), service = service.name, "registering a service in the prefix");
        let mut launch = Launch::new("sc", format!("{}/service", self.path.display()))
            .game_id("umu-decanter")
            .prefix(self.clone())
            .proton(proton.clone())
            .arg("create")
            .arg(service.name)
            .arg("binPath=")
            .arg(format!("C:\\{}\\{file_name}", PROGRAM_DATA_DIR.join("\\")))
            .arg("start=")
            .arg("demand");
        launch.working_directory = Some(env::temp_dir());
        let session = crate::start(launch).await.context("starting sc")?;
        session.wait().await.context("registering the service with sc create")?;

        tokio::fs::write(&marker, [])
            .await
            .with_context(|| format!("writing {}", marker.display()))?;
        Ok(true)
    }
}
