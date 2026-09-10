//! The views module contains the components for all Layouts and Routes for our app.

mod library;
pub use library::Library;

mod settings;
pub use settings::Settings;

mod storage;
pub use storage::Storage;

pub(crate) mod navbar;
pub use navbar::Navbar;

mod downloads;
pub use downloads::Downloads;

mod onboarding;
pub use onboarding::Onboarding;
