pub mod application;
pub mod artwork;
pub mod calendar;
pub mod config;
pub mod error;
pub mod images;
pub mod metadata;
pub mod migration;
pub mod model;
pub mod prompts;
pub mod publish_journal;
pub mod publish_protocol;
pub mod publishing;
pub mod scheduling;
pub mod store;
#[cfg(feature = "desktop")]
pub mod ui;
pub mod workflow;

#[cfg(test)]
#[path = "../tests/common/executable.rs"]
mod executable_fixture;
#[cfg(test)]
#[path = "../tests/common/git.rs"]
mod git_fixture;
#[cfg(test)]
#[path = "../tests/common/image.rs"]
mod image_fixture;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../tests/common/process.rs"]
mod process_fixture;
