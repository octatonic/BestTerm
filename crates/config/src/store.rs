//! Reading and writing versioned TOML documents.
//!
//! Three properties, each earned by a failure mode worth avoiding:
//!
//! * **Versioned.** Every file carries a `version`. A build that meets a file from a *newer* build
//!   refuses to read it rather than dropping the keys it does not recognise — silently discarding a
//!   user's settings because they ran an older binary once is unacceptable.
//! * **Migrated forward, with a backup.** Older files are stepped up one version at a time, and the
//!   original is copied aside first. A migration bug must not be able to destroy a session tree.
//! * **Written atomically.** Write to a temporary file, flush it to disk, then rename over the
//!   target. A crash or a full disk half-way through leaves the previous file intact instead of a
//!   truncated one.

use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use toml::{Table, Value};

/// The key a document's schema version lives under.
const VERSION_KEY: &str = "version";

/// Things that can go wrong loading or saving.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// File involved.
        path: PathBuf,
        /// Underlying cause.
        source: std::io::Error,
    },

    /// The file is not valid TOML, or does not match the expected shape.
    #[error("{path}: {source}")]
    Parse {
        /// File involved.
        path: PathBuf,
        /// Underlying cause.
        source: toml::de::Error,
    },

    /// The value could not be turned into TOML.
    #[error("could not serialise {name}: {source}")]
    Serialize {
        /// Document kind.
        name: &'static str,
        /// Underlying cause.
        source: toml::ser::Error,
    },

    /// The file was written by a newer BestTerm.
    #[error(
        "{path} was written by a newer version of BestTerm (schema {found}, this build understands \
         {supported}); it has been left untouched"
    )]
    FromTheFuture {
        /// File involved.
        path: PathBuf,
        /// Version found in the file.
        found: u32,
        /// Highest version this build understands.
        supported: u32,
    },

    /// No migration exists for a version this build should be able to upgrade.
    #[error("no migration from schema {from} for {name}; this is a bug in BestTerm")]
    MissingMigration {
        /// Document kind.
        name: &'static str,
        /// Version with no step away from it.
        from: u32,
    },

    /// The file parsed, but does not describe a valid session tree.
    #[error("{path}: {source}")]
    InvalidTree {
        /// File involved.
        path: PathBuf,
        /// What is wrong with the structure.
        source: bestterm_core_model::DocError,
    },

    /// A migration step refused the file.
    #[error("migrating {name} from schema {from} failed: {detail}")]
    Migration {
        /// Document kind.
        name: &'static str,
        /// Step that failed.
        from: u32,
        /// What the step complained about.
        detail: String,
    },
}

/// Result alias for configuration I/O.
pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

/// One step, taking a document from `from` to `from + 1`.
///
/// Steps work on the raw [`Table`] rather than on typed values, because the type they would need is
/// the *old* shape — which no longer exists in the code by the time the migration is written.
pub struct Migration {
    /// Version this step upgrades from.
    pub from: u32,
    /// What it does, for the log.
    pub summary: &'static str,
    /// The transformation. Returns a human-readable complaint on refusal.
    pub apply: fn(&mut Table) -> std::result::Result<(), String>,
}

/// A document persisted as versioned TOML.
pub trait Document: Serialize + DeserializeOwned + Default {
    /// Schema version this build writes.
    const VERSION: u32;

    /// Name used in messages.
    const NAME: &'static str;

    /// Steps from older versions up to [`Self::VERSION`], in any order.
    fn migrations() -> &'static [Migration] {
        &[]
    }
}

/// Load a document, migrating it forward if it is older.
pub fn load<T: Document>(path: &Path) -> ConfigResult<T> {
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let mut table: Table = toml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })?;

    // A file with no version predates versioning, which means the first release.
    let found = table
        .get(VERSION_KEY)
        .and_then(Value::as_integer)
        .and_then(|version| u32::try_from(version).ok())
        .unwrap_or(1);

    if found > T::VERSION {
        return Err(ConfigError::FromTheFuture {
            path: path.to_path_buf(),
            found,
            supported: T::VERSION,
        });
    }

    if found < T::VERSION {
        // Before anything is transformed. The migrated form only reaches disk on the next save, so
        // this copy is what a user recovers from if a step turns out to be wrong.
        back_up(path, found)?;

        let mut version = found;
        while version < T::VERSION {
            let step = T::migrations()
                .iter()
                .find(|migration| migration.from == version)
                .ok_or(ConfigError::MissingMigration {
                    name: T::NAME,
                    from: version,
                })?;

            (step.apply)(&mut table).map_err(|detail| ConfigError::Migration {
                name: T::NAME,
                from: version,
                detail,
            })?;

            tracing::info!(
                document = T::NAME,
                from = version,
                to = version + 1,
                summary = step.summary,
                "migrated configuration"
            );
            version += 1;
        }
    }

    // The version is the store's business, not the document's, so it never reaches the type.
    table.remove(VERSION_KEY);

    Value::Table(table)
        .try_into()
        .map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
}

/// Load a document, or its default when the file does not exist.
///
/// A missing file is the first-run case, not an error. Anything else — unreadable, malformed, from
/// the future — is reported, because silently replacing a file that exists but cannot be understood
/// would destroy it on the next save.
pub fn load_or_default<T: Document>(path: &Path) -> ConfigResult<T> {
    match load(path) {
        Ok(value) => Ok(value),
        Err(ConfigError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(document = T::NAME, ?path, "no file yet; using defaults");
            Ok(T::default())
        }
        Err(other) => Err(other),
    }
}

/// Write a document, atomically.
pub fn save<T: Document>(path: &Path, value: &T) -> ConfigResult<()> {
    let mut table = Table::try_from(value).map_err(|source| ConfigError::Serialize {
        name: T::NAME,
        source,
    })?;
    table.insert(
        VERSION_KEY.to_string(),
        Value::Integer(i64::from(T::VERSION)),
    );

    let text = toml::to_string_pretty(&table).map_err(|source| ConfigError::Serialize {
        name: T::NAME,
        source,
    })?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let temporary = temporary_path(path);
    let write_result = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        // Flush to the device before the rename, so a crash cannot leave the new name pointing at
        // an empty file.
        file.sync_all()?;
        Ok(())
    })();

    if let Err(source) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(ConfigError::Io {
            path: temporary,
            source,
        });
    }

    // `fs::rename` replaces the destination on both platforms.
    if let Err(source) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        });
    }

    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    sibling(path, ".tmp")
}

fn back_up(path: &Path, from_version: u32) -> ConfigResult<()> {
    let backup = sibling(path, &format!(".v{from_version}.bak"));
    fs::copy(path, &backup).map_err(|source| ConfigError::Io {
        path: backup,
        source,
    })?;
    Ok(())
}

/// `path` with `suffix` appended to its file name.
///
/// Appended rather than replacing the extension, so `sessions.toml` becomes `sessions.toml.v1.bak`
/// and stays recognisable next to the file it came from.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    /// A document at version 3, upgradable from 1.
    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Sample {
        greeting: String,
        count: u32,
    }

    static MIGRATIONS: &[Migration] = &[
        Migration {
            from: 1,
            summary: "rename `hello` to `greeting`",
            apply: |table| {
                if let Some(value) = table.remove("hello") {
                    table.insert("greeting".to_string(), value);
                }
                Ok(())
            },
        },
        Migration {
            from: 2,
            summary: "add `count`",
            apply: |table| {
                table
                    .entry("count".to_string())
                    .or_insert(Value::Integer(7));
                Ok(())
            },
        },
    ];

    impl Document for Sample {
        const VERSION: u32 = 3;
        const NAME: &'static str = "sample";

        fn migrations() -> &'static [Migration] {
            MIGRATIONS
        }
    }

    /// A document whose only migration always refuses.
    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct Stubborn {
        value: u32,
    }

    static REFUSING: &[Migration] = &[Migration {
        from: 1,
        summary: "always fails",
        apply: |_| Err("nope".to_string()),
    }];

    impl Document for Stubborn {
        const VERSION: u32 = 2;
        const NAME: &'static str = "stubborn";

        fn migrations() -> &'static [Migration] {
            REFUSING
        }
    }

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    #[test]
    fn a_saved_document_reloads_unchanged() {
        let dir = temp();
        let path = dir.path().join("sample.toml");
        let original = Sample {
            greeting: "hei".to_string(),
            count: 3,
        };

        save(&path, &original).expect("saves");
        let loaded: Sample = load(&path).expect("loads");
        assert_eq!(loaded, original);
    }

    #[test]
    fn the_version_is_written_but_never_reaches_the_type() {
        let dir = temp();
        let path = dir.path().join("sample.toml");
        save(&path, &Sample::default()).expect("saves");

        let text = fs::read_to_string(&path).expect("reads");
        assert!(text.contains("version = 3"), "got:\n{text}");

        // `Sample` denies unknown fields, so loading proves the store strips the key rather than
        // handing it to the document.
        load::<Sample>(&path).expect("loads");
    }

    #[test]
    fn saving_creates_missing_directories() {
        let dir = temp();
        let path = dir.path().join("nested").join("deeper").join("sample.toml");
        save(&path, &Sample::default()).expect("saves");
        assert!(path.exists());
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let dir = temp();
        let path = dir.path().join("sample.toml");
        save(&path, &Sample::default()).expect("saves");
        assert!(!temporary_path(&path).exists());
    }

    #[test]
    fn saving_over_an_existing_file_replaces_it() {
        let dir = temp();
        let path = dir.path().join("sample.toml");
        save(
            &path,
            &Sample {
                greeting: "first".to_string(),
                count: 1,
            },
        )
        .expect("saves");
        save(
            &path,
            &Sample {
                greeting: "second".to_string(),
                count: 2,
            },
        )
        .expect("saves again");

        let loaded: Sample = load(&path).expect("loads");
        assert_eq!(loaded.greeting, "second");
    }

    #[test]
    fn a_missing_file_yields_the_default() {
        let dir = temp();
        let path = dir.path().join("absent.toml");
        let loaded: Sample = load_or_default(&path).expect("defaults");
        assert_eq!(loaded, Sample::default());
        // Reading must not create the file.
        assert!(!path.exists());
    }

    #[test]
    fn a_malformed_file_is_reported_not_replaced() {
        // Quietly falling back to defaults here would destroy the file on the next save.
        let dir = temp();
        let path = dir.path().join("broken.toml");
        fs::write(&path, "this is not = = toml").expect("writes");

        let error = load_or_default::<Sample>(&path).expect_err("must fail");
        assert!(matches!(error, ConfigError::Parse { .. }), "got {error:?}");
        assert!(path.exists());
    }

    #[test]
    fn an_old_file_is_migrated_step_by_step() {
        let dir = temp();
        let path = dir.path().join("sample.toml");
        fs::write(&path, "version = 1\nhello = \"moi\"\n").expect("writes");

        let loaded: Sample = load(&path).expect("migrates and loads");
        assert_eq!(loaded.greeting, "moi", "step 1->2 should rename the key");
        assert_eq!(loaded.count, 7, "step 2->3 should supply the new field");
    }

    #[test]
    fn a_file_with_no_version_is_treated_as_the_first_schema() {
        let dir = temp();
        let path = dir.path().join("sample.toml");
        fs::write(&path, "hello = \"pre-versioning\"\n").expect("writes");

        let loaded: Sample = load(&path).expect("migrates and loads");
        assert_eq!(loaded.greeting, "pre-versioning");
    }

    #[test]
    fn migrating_backs_up_the_original_first() {
        let dir = temp();
        let path = dir.path().join("sample.toml");
        let before = "version = 1\nhello = \"precious\"\n";
        fs::write(&path, before).expect("writes");

        load::<Sample>(&path).expect("migrates");

        let backup = sibling(&path, ".v1.bak");
        assert!(backup.exists(), "the pre-migration file must be kept");
        assert_eq!(fs::read_to_string(&backup).expect("reads"), before);
    }

    #[test]
    fn a_file_from_the_future_is_refused_and_left_alone() {
        let dir = temp();
        let path = dir.path().join("sample.toml");
        let before = "version = 99\ngreeting = \"from tomorrow\"\nunknown_key = 1\n";
        fs::write(&path, before).expect("writes");

        let error = load::<Sample>(&path).expect_err("must fail");
        match error {
            ConfigError::FromTheFuture {
                found, supported, ..
            } => {
                assert_eq!(found, 99);
                assert_eq!(supported, 3);
            }
            other => panic!("expected FromTheFuture, got {other:?}"),
        }
        // Untouched, so a newer build can still read it.
        assert_eq!(fs::read_to_string(&path).expect("reads"), before);
    }

    #[test]
    fn a_failing_migration_is_reported_with_its_complaint() {
        let dir = temp();
        let path = dir.path().join("stubborn.toml");
        fs::write(&path, "version = 1\nvalue = 1\n").expect("writes");

        let error = load::<Stubborn>(&path).expect_err("must fail");
        match error {
            ConfigError::Migration { from, detail, .. } => {
                assert_eq!(from, 1);
                assert_eq!(detail, "nope");
            }
            other => panic!("expected Migration, got {other:?}"),
        }
    }

    #[test]
    fn a_gap_in_the_migration_chain_is_a_reported_bug_not_a_silent_skip() {
        #[derive(Debug, Default, Serialize, Deserialize)]
        struct Gappy {}

        impl Document for Gappy {
            const VERSION: u32 = 5;
            const NAME: &'static str = "gappy";
            // No migrations at all, so version 1 cannot reach 5.
        }

        let dir = temp();
        let path = dir.path().join("gappy.toml");
        fs::write(&path, "version = 1\n").expect("writes");

        let error = load::<Gappy>(&path).expect_err("must fail");
        assert!(
            matches!(error, ConfigError::MissingMigration { from: 1, .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn a_backup_sits_beside_the_file_it_came_from() {
        let path = Path::new("/cfg/sessions.toml");
        assert_eq!(
            sibling(path, ".v2.bak"),
            Path::new("/cfg/sessions.toml.v2.bak")
        );
        assert_eq!(temporary_path(path), Path::new("/cfg/sessions.toml.tmp"));
    }
}
