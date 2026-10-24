//! A job as the editor holds it: text the user is typing, turned into a [`Job`] on save.

use std::collections::HashSet;
use std::path::PathBuf;

use replica_core::job::{AfterStep, BeforeStep, DEFAULT_TIMEOUT_SECS, Job, Mode, Source, Target};
use replica_core::job::{DEFAULT_READ_DATA_SUBSET, Retention, VolumeRef};
use replica_core::restic::password_secret_name;
use replica_core::schedule::Schedule;
use replica_win::targets::share_root;

#[derive(Debug, Clone, PartialEq)]
pub struct JobForm {
    pub name: String,
    pub enabled: bool,
    pub mode: Mode,
    pub mirror_deletes_confirmed: bool,
    pub mirror_delete_limit_percent: u32,
    pub schedule: String,
    pub notify_on_success: bool,
    pub retention: Retention,
    pub sources: Vec<SourceForm>,
    pub targets: Vec<TargetForm>,
    pub before: Vec<BeforeForm>,
    pub after: Vec<AfterForm>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceForm {
    pub path: String,
    /// Exclusions, one per line.
    pub excludes: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetKind {
    Drive,
    Share,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetForm {
    pub kind: TargetKind,
    pub name: String,
    /// A drive's volume.
    pub volume: VolumeRef,
    /// A drive's folder (relative to the volume's root), or a share's UNC path.
    pub path: String,
    /// The share login's name in Credential Manager, if it has one.
    pub credential: Option<String>,
    /// A login being typed: saved under [`login_name`] when the job is.
    pub login_user: String,
    pub login_password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeforeForm {
    Sqlite { db: String },
    Command { cmd: String, timeout_secs: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AfterForm {
    Command { cmd: String, timeout_secs: u64 },
    Verify { schedule: String, subset: String },
}

impl TargetForm {
    pub fn drive() -> Self {
        Self {
            kind: TargetKind::Drive,
            name: String::new(),
            volume: VolumeRef::default(),
            path: "Replica".into(),
            credential: None,
            login_user: String::new(),
            login_password: String::new(),
        }
    }

    pub fn share() -> Self {
        Self {
            kind: TargetKind::Share,
            path: String::new(),
            ..Self::drive()
        }
    }

    /// Whether a login is being typed (it replaces a saved one on save).
    pub fn has_new_login(&self) -> bool {
        !self.login_user.trim().is_empty() || !self.login_password.is_empty()
    }

    pub fn to_target(&self) -> Target {
        let name = Some(self.name.trim().to_owned()).filter(|n| !n.is_empty());
        match self.kind {
            TargetKind::Drive => Target::Drive {
                name,
                volume: self.volume.clone(),
                path: PathBuf::from(self.path.trim()),
            },
            TargetKind::Share => {
                let path = self.path.trim().to_owned();
                let credential = if self.has_new_login() {
                    Some(login_name(&path))
                } else {
                    self.credential.clone()
                };
                Target::Unc {
                    name,
                    path,
                    credential,
                }
            }
        }
    }

    fn from_target(target: &Target) -> Self {
        match target {
            Target::Drive { name, volume, path } => Self {
                name: name.clone().unwrap_or_default(),
                volume: volume.clone(),
                path: path.to_string_lossy().into_owned(),
                ..Self::drive()
            },
            Target::Unc {
                name,
                path,
                credential,
            } => Self {
                name: name.clone().unwrap_or_default(),
                path: path.clone(),
                credential: credential.clone(),
                ..Self::share()
            },
        }
    }
}

/// The Credential Manager name of a share's login: one per share, so every target on it uses
/// the same login (`share:\\nas\backup`).
pub fn login_name(unc: &str) -> String {
    let root = share_root(unc.trim()).map_or_else(|| unc.trim().to_owned(), |(_, root)| root);
    format!("share:{}", root.to_lowercase())
}

impl Default for JobForm {
    fn default() -> Self {
        Self::from_job(&Job::default())
    }
}

impl JobForm {
    pub fn from_job(job: &Job) -> Self {
        Self {
            name: job.name.clone(),
            enabled: job.enabled,
            mode: job.mode,
            mirror_deletes_confirmed: job.mirror_deletes_confirmed,
            mirror_delete_limit_percent: job.mirror_delete_limit_percent,
            schedule: job
                .schedule
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            notify_on_success: job.notify_on_success,
            retention: job.retention,
            sources: job
                .sources
                .iter()
                .map(|s| SourceForm {
                    path: s.path.to_string_lossy().into_owned(),
                    excludes: s.exclude.join("\n"),
                })
                .collect(),
            targets: job.targets.iter().map(TargetForm::from_target).collect(),
            before: job
                .before
                .iter()
                .map(|s| match s {
                    BeforeStep::SqliteSnapshot { db } => BeforeForm::Sqlite {
                        db: db.to_string_lossy().into_owned(),
                    },
                    BeforeStep::Command { cmd, timeout_secs } => BeforeForm::Command {
                        cmd: cmd.clone(),
                        timeout_secs: *timeout_secs,
                    },
                })
                .collect(),
            after: job
                .after
                .iter()
                .map(|s| match s {
                    AfterStep::Command { cmd, timeout_secs } => AfterForm::Command {
                        cmd: cmd.clone(),
                        timeout_secs: *timeout_secs,
                    },
                    AfterStep::Verify {
                        schedule,
                        read_data_subset,
                    } => AfterForm::Verify {
                        schedule: schedule
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                        subset: read_data_subset.clone(),
                    },
                })
                .collect(),
        }
    }

    /// A new source for `path`, with the common exclusions, as a new job starts.
    pub fn add_source(&mut self, path: PathBuf) {
        let source = Source::with_common_excludes(path);
        self.sources.push(SourceForm {
            path: source.path.to_string_lossy().into_owned(),
            excludes: source.exclude.join("\n"),
        });
    }

    pub fn new_command() -> (String, u64) {
        (String::new(), DEFAULT_TIMEOUT_SECS)
    }

    pub fn new_verify() -> AfterForm {
        AfterForm::Verify {
            schedule: String::new(),
            subset: DEFAULT_READ_DATA_SUBSET.into(),
        }
    }

    /// The job, or the schedules that don't parse (core's own messages). Everything else is
    /// for [`replica_core::job::validate_jobs`] to judge.
    pub fn to_job(&self) -> Result<Job, Vec<String>> {
        let mut errors = Vec::new();
        let mut schedule = |text: &str| -> Option<Schedule> {
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            text.parse::<Schedule>()
                .map_err(|e| errors.push(e.to_string()))
                .ok()
        };
        let job_schedule = schedule(&self.schedule);
        let after = self
            .after
            .iter()
            .map(|s| match s {
                AfterForm::Command { cmd, timeout_secs } => AfterStep::Command {
                    cmd: cmd.trim().to_owned(),
                    timeout_secs: *timeout_secs,
                },
                AfterForm::Verify {
                    schedule: s,
                    subset,
                } => AfterStep::Verify {
                    schedule: schedule(s),
                    read_data_subset: subset.trim().to_owned(),
                },
            })
            .collect();
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Job {
            name: self.name.trim().to_owned(),
            enabled: self.enabled,
            mode: self.mode,
            mirror_deletes_confirmed: self.mode == Mode::Mirror && self.mirror_deletes_confirmed,
            mirror_delete_limit_percent: self.mirror_delete_limit_percent,
            schedule: job_schedule,
            notify_on_success: self.notify_on_success,
            retention: self.retention,
            sources: self
                .sources
                .iter()
                .map(|s| Source {
                    path: PathBuf::from(s.path.trim()),
                    exclude: s
                        .excludes
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_owned)
                        .collect(),
                })
                .collect(),
            targets: self.targets.iter().map(TargetForm::to_target).collect(),
            before: self
                .before
                .iter()
                .map(|s| match s {
                    BeforeForm::Sqlite { db } => BeforeStep::SqliteSnapshot {
                        db: PathBuf::from(db.trim()),
                    },
                    BeforeForm::Command { cmd, timeout_secs } => BeforeStep::Command {
                        cmd: cmd.trim().to_owned(),
                        timeout_secs: *timeout_secs,
                    },
                })
                .collect(),
            after,
        })
    }
}

/// The versioned targets of `job` that have to be set up (a repository made and its password
/// shown) when it's saved: those that weren't versioned targets of `before`, the job as it was.
pub fn targets_to_set_up(job: &Job, before: Option<&Job>) -> Vec<Target> {
    if job.mode != Mode::Versioned {
        return Vec::new();
    }
    let known: HashSet<String> = before
        .filter(|b| b.mode == Mode::Versioned)
        .map(|b| b.targets.iter().map(password_secret_name).collect())
        .unwrap_or_default();
    job.targets
        .iter()
        .filter(|t| !known.contains(&password_secret_name(t)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Job {
        Job {
            name: "hive".into(),
            schedule: Some("daily 02:30".parse().unwrap()),
            sources: vec![Source {
                path: r"C:\Work".into(),
                exclude: vec!["node_modules".into(), "*.tmp".into()],
            }],
            targets: vec![
                Target::Drive {
                    name: Some("external".into()),
                    volume: VolumeRef {
                        volume_id: Some(r"\\?\Volume{1}\".into()),
                        label: Some("BACKUP".into()),
                    },
                    path: "Replica".into(),
                },
                Target::Unc {
                    name: None,
                    path: r"\\nas\backup\hive".into(),
                    credential: Some(r"share:\\nas\backup".into()),
                },
            ],
            before: vec![
                BeforeStep::SqliteSnapshot {
                    db: r"C:\Work\a.db".into(),
                },
                BeforeStep::Command {
                    cmd: "echo hi".into(),
                    timeout_secs: 60,
                },
            ],
            after: vec![AfterStep::Verify {
                schedule: Some("weekly sun 03:00".parse().unwrap()),
                read_data_subset: "5%".into(),
            }],
            ..Job::default()
        }
    }

    #[test]
    fn a_job_round_trips_through_the_form() {
        let job = sample();
        assert_eq!(JobForm::from_job(&job).to_job().unwrap(), job);
    }

    #[test]
    fn a_bad_schedule_is_core_s_message() {
        let mut form = JobForm::from_job(&sample());
        form.schedule = "sometimes".into();
        let errors = form.to_job().unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("sometimes"), "{errors:?}");
    }

    #[test]
    fn excludes_are_one_per_line_and_blank_lines_go() {
        let mut form = JobForm::default();
        form.sources.push(SourceForm {
            path: r" C:\A ".into(),
            excludes: "a\n\n  b  \n".into(),
        });
        let job = form.to_job().unwrap();
        assert_eq!(job.sources[0].path, PathBuf::from(r"C:\A"));
        assert_eq!(job.sources[0].exclude, ["a", "b"]);
    }

    #[test]
    fn a_typed_login_names_the_share_s_credential() {
        let mut target = TargetForm::share();
        target.path = r"\\NAS\Backup\hive".into();
        assert!(matches!(
            target.to_target(),
            Target::Unc {
                credential: None,
                ..
            }
        ));
        target.login_user = "alex".into();
        match target.to_target() {
            Target::Unc { credential, .. } => {
                assert_eq!(credential.as_deref(), Some(r"share:\\nas\backup"))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mirror_confirmation_only_counts_for_a_mirror() {
        let mut form = JobForm::from_job(&sample());
        form.mirror_deletes_confirmed = true;
        assert!(!form.to_job().unwrap().mirror_deletes_confirmed);
        form.mode = Mode::Mirror;
        assert!(form.to_job().unwrap().mirror_deletes_confirmed);
    }

    #[test]
    fn only_new_versioned_targets_are_set_up() {
        let before = sample();
        let mut job = sample();
        assert!(targets_to_set_up(&job, Some(&before)).is_empty());
        assert_eq!(targets_to_set_up(&job, None).len(), 2);

        job.targets.push(Target::Unc {
            name: None,
            path: r"\\nas\other".into(),
            credential: None,
        });
        let new = targets_to_set_up(&job, Some(&before));
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].display_name(), r"\\nas\other");

        // Mirror to versioned: every target is new as a repository.
        let mirror = Job {
            mode: Mode::Mirror,
            ..before.clone()
        };
        assert_eq!(targets_to_set_up(&sample(), Some(&mirror)).len(), 2);
        // A mirror needs none.
        assert!(targets_to_set_up(&mirror, None).is_empty());
    }
}
