//! Folder upload persistence: one job row per folder plus one item row per walked entry.
//! Timestamps are unix seconds; `mtime_ms` and `remote_mtime` are milliseconds.

mod counts;
mod items;
mod jobs;
mod remote;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

pub use items::{ItemCounts, ItemRow, NewItem, MAX_KEY_BYTES};
pub use jobs::{JobRow, NewJob};

// String-backed enum stored as TEXT; as_str/parse plus rusqlite ToSql/FromSql.
macro_rules! str_enum {
    ($(#[$m:meta])* $name:ident { $($var:ident => $s:literal),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $s)] $var,)*
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$var,)*];

            pub fn as_str(&self) -> &'static str {
                match self { $($name::$var => $s,)* }
            }

            pub fn parse(s: &str) -> AppResult<Self> {
                match s {
                    $($s => Ok($name::$var),)*
                    other => Err(AppError::InvalidInput(format!(concat!(stringify!($name), ": {}"), other))),
                }
            }
        }

        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }

        impl FromSql for $name {
            fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
                Self::parse(v.as_str()?).map_err(|e| FromSqlError::Other(Box::new(e)))
            }
        }
    };
}

str_enum!(JobState {
    Analyzing => "analyzing",
    Analyzed => "analyzed",
    Queued => "queued",
    Running => "running",
    Paused => "paused",
    Completed => "completed",
    CompletedWithErrors => "completed_with_errors",
    Canceled => "canceled",
    Failed => "failed",
});

impl JobState {
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Completed | Self::CompletedWithErrors | Self::Canceled | Self::Failed)
    }

    /// Approved and not finished: queued, running or paused.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Paused)
    }
}

str_enum!(ItemKind {
    File => "file",
    Dir => "dir",
    Symlink => "symlink",
    Special => "special",
});

str_enum!(ItemState {
    Pending => "pending",
    Uploading => "uploading",
    Done => "done",
    SkippedExists => "skipped_exists",
    SkippedMissing => "skipped_missing",
    Failed => "failed",
    Canceled => "canceled",
    Excluded => "excluded",
});

str_enum!(OverwritePolicy {
    Skip => "skip",
    Overwrite => "overwrite",
});

str_enum!(ItemFilter {
    All => "all",
    Existing => "existing",
    Excluded => "excluded",
    Failed => "failed",
    Skipped => "skipped",
    Drift => "drift",
    Hidden => "hidden",
});

str_enum!(ExcludeReason {
    Symlink => "symlink",
    Special => "special",
    Unreadable => "unreadable",
    UnreadableDir => "unreadable_dir",
    NonUtf8 => "non_utf8",
    ControlChars => "control_chars",
    KeyTooLong => "key_too_long",
    FileTooLarge => "file_too_large",
});

str_enum!(Drift {
    Changed => "changed",
    ChangedDuringUpload => "changed_during_upload",
});

str_enum!(RemoteState {
    Pending => "pending",
    Checking => "checking",
    Done => "done",
    Failed => "failed",
});

pub(crate) const FINISHED_JOB_STATES: &str = "('completed','completed_with_errors','canceled','failed')";
pub(crate) const ACTIVE_JOB_STATES: &str = "('queued','running','paused')";
