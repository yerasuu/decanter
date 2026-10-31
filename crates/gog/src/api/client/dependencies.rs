// SPDX-License-Identifier: GPL-3.0-or-later
//! GOG's dependency repository: the redistributables (the Visual C++ runtimes and the like) a
//! build names in its `dependencies`, each a depot of its own with the installer to run.
//!
//! GOG documents none of it. The flow mirrors heroic-gogdl, which Heroic installs redistributables
//! with: the [repository and its depot manifests](https://github.com/Heroic-Games-Launcher/heroic-gogdl/blob/main/gogdl/dl/managers/dependencies.py),
//! the [repository URL](https://github.com/Heroic-Games-Launcher/heroic-gogdl/blob/main/gogdl/constants.py)
//! and the [link chunks are signed with](https://github.com/Heroic-Games-Launcher/heroic-gogdl/blob/main/gogdl/dl/dl_utils.py)
//! (`open_link`, product-less, under `/dependencies/store/`).

use serde::Deserialize;

use super::Error;
use super::content::{
    DepotFile, SecureLinkAnswer, SecureLinkEntry, describe_link, inflated, is_dependency_id, is_manifest_id,
    manifest_host,
};
use super::http::{self, Call, Host, SESSION_REFUSED, Sessionless};

const REPOSITORY: Call = Call {
    name: "dependency repository",
    refused: SESSION_REFUSED,
    max_body: 1 << 20,
    has_secrets: false,
};

const REPOSITORY_MANIFEST: Call = Call {
    name: "dependency repository manifest",
    refused: &[],
    max_body: 4 << 20,
    has_secrets: false,
};

const REPOSITORY_MANIFEST_INFLATED: usize = 16 << 20;

/// Its URLs carry signed CDN tokens.
const DEPENDENCY_LINK: Call = Call {
    name: "dependency link",
    refused: SESSION_REFUSED,
    max_body: 1 << 20,
    has_secrets: true,
};

#[derive(Deserialize)]
struct RepositoryAnswer {
    repository_manifest: String,
}

#[derive(Deserialize)]
struct RepositoryManifest {
    #[serde(default)]
    depots: Vec<RawDependency>,
}

#[derive(Deserialize)]
struct RawDependency {
    #[serde(rename = "dependencyId")]
    dependency_id: String,
    #[serde(rename = "readableName", default)]
    readable_name: Option<String>,
    manifest: String,
    #[serde(default)]
    executable: Option<RawExecutable>,
}

#[derive(Deserialize)]
struct RawExecutable {
    #[serde(default)]
    path: String,
    #[serde(default)]
    arguments: String,
}

/// One redistributable of the repository.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Dependency {
    pub id: String,
    /// GOG's name for it (`Microsoft Visual C++ 2015 Redistributable`), else its id.
    pub name: String,
    /// Its depot manifest's id, for [`Sessionless::dependency_files`].
    pub manifest: String,
    /// The installer, relative to where the depot's files land, with Windows separators as GOG
    /// lists it; empty for a dependency with nothing to run.
    pub executable: String,
    /// The installer's arguments as one command line (`/install /quiet /norestart`).
    pub arguments: String,
}

impl Sessionless {
    /// Every redistributable GOG's dependency repository offers.
    ///
    /// An entry whose id or manifest isn't one is left out, logged, so the rest still install.
    ///
    /// # Errors
    ///
    /// [`Error::Rejected`] when GOG refuses `access_token`; [`Error::Malformed`] for a manifest
    /// link that is not an `https` URL on a GOG host, or a manifest that is not zlib'd JSON within
    /// its size caps; the others when GOG cannot be asked.
    #[inline]
    pub async fn dependency_repository(&self, access_token: &str) -> Result<Vec<Dependency>, Error> {
        let answer: RepositoryAnswer = self
            .get(
                &REPOSITORY,
                Host::ContentSystem,
                "/dependencies/repository",
                &[("generation", "2")],
                Some(access_token),
            )
            .await?;
        let link = answer.repository_manifest;
        let (host, is_gog_com) = manifest_host(&link).ok_or_else(|| Error::Malformed {
            call: REPOSITORY_MANIFEST.name,
            reason: format!("link is not an https URL on a GOG host ({})", describe_link(&link)),
        })?;
        let bearer = is_gog_com.then_some(access_token);
        let compressed = self.get_bytes(&REPOSITORY_MANIFEST, &link, &host, &[], bearer).await?;
        let manifest: RepositoryManifest = inflated(&REPOSITORY_MANIFEST, &compressed, REPOSITORY_MANIFEST_INFLATED)?;
        Ok(manifest
            .depots
            .into_iter()
            .filter_map(|raw| {
                if !is_dependency_id(&raw.dependency_id) || !is_manifest_id(&raw.manifest) {
                    tracing::warn!(
                        dependency = raw.dependency_id,
                        "dependency repository entry with an unusable id or manifest, skipped"
                    );
                    return None;
                }
                let (executable, arguments) = raw.executable.map(|exe| (exe.path, exe.arguments)).unwrap_or_default();
                Some(Dependency {
                    name: raw.readable_name.unwrap_or_else(|| raw.dependency_id.clone()),
                    id: raw.dependency_id,
                    manifest: raw.manifest,
                    executable,
                    arguments,
                })
            })
            .collect())
    }

    /// Every file of one redistributable's depot.
    ///
    /// # Errors
    ///
    /// As [`Sessionless::depot_files`].
    #[inline]
    pub async fn dependency_files(&self, manifest: &str) -> Result<Vec<DepotFile>, Error> {
        if !is_manifest_id(manifest) {
            return Err(Error::InvalidInput {
                what: "dependency manifest id",
            });
        }
        self.manifest_files("/content-system/v2/dependencies/meta", manifest)
            .await
    }

    /// The CDN endpoints redistributables' chunk URLs are built from; unlike a game's, they are
    /// signed for no product.
    ///
    /// # Errors
    ///
    /// As [`Sessionless::builds`].
    #[inline]
    pub async fn dependency_link(&self, access_token: &str) -> Result<Vec<SecureLinkEntry>, Error> {
        let url = Host::ContentSystem.url("/open_link?generation=2&_version=2&path=/dependencies/store/");
        let body = self
            .get_bytes(
                &DEPENDENCY_LINK,
                &url,
                Host::ContentSystem.name(),
                &[],
                Some(access_token),
            )
            .await?;
        let answer: SecureLinkAnswer = http::parse(&DEPENDENCY_LINK, &body)?;
        Ok(answer.urls)
    }
}
