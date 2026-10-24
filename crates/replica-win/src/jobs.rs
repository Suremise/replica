//! The process trees of command steps and restic commands: Windows job objects.
//!
//! [`JobObjectTree`] is `replica-core`'s [`ProcessTree`] for Windows: command steps run in it,
//! and so does every restic command (`Restic::with_tree`, in the CLI and the tray). Each program
//! it starts is created suspended, put in a new job object with kill-on-close, and only then
//! resumed, so nothing it starts can escape the job. Stopping the tree terminates the job, which
//! ends every process in it, and waits until the job itself reports none left. Closing the job
//! (dropping the child) ends anything still in it, so neither a step's programs nor restic
//! outlive their step or command, even if Replica stops. Failures are errors, never ignored.

use std::io;
use std::os::windows::io::AsRawHandle;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus};
use std::time::{Duration, Instant};

use replica_core::platform::{ProcessTree, TreeChild};

use crate::win32::{self, Job};

/// `CREATE_SUSPENDED`: the program doesn't run until it's in its job.
const CREATE_SUSPENDED: u32 = 0x0000_0004;

/// How long stopping a tree waits for the job to report no processes left.
const STOP_LIMIT: Duration = Duration::from_secs(30);

/// Runs programs in Windows job objects with kill-on-close.
#[derive(Debug, Default, Clone, Copy)]
pub struct JobObjectTree;

impl ProcessTree for JobObjectTree {
    fn spawn(&self, mut command: Command) -> io::Result<Box<dyn TreeChild>> {
        // Added to the command's flags, never a console window: `creation_flags` alone
        // would replace them.
        replica_core::process::set_creation_flags(&mut command, CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        let in_job = Job::new_kill_on_close().and_then(|job| {
            job.assign(child.as_raw_handle())?;
            win32::resume_process(child.id())?;
            Ok(job)
        });
        match in_job {
            Ok(job) => Ok(Box::new(JobChild { child, job })),
            Err(e) => {
                // Never leave it running outside a job.
                let _ = child.kill();
                let _ = child.wait();
                Err(e)
            }
        }
    }
}

/// A program in its job.
struct JobChild {
    child: Child,
    job: Job,
}

impl TreeChild for JobChild {
    fn id(&self) -> u32 {
        self.child.id()
    }

    fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    fn kill_tree(&mut self) -> io::Result<()> {
        self.job.terminate(1)?;
        let start = Instant::now();
        loop {
            if self.job.active_processes()? == 0 {
                break;
            }
            if start.elapsed() > STOP_LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "the job still has processes {:?} after terminating",
                        self.job.process_ids()?
                    ),
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.child.wait().map(|_| ())
    }

    fn processes(&self) -> io::Result<Vec<u32>> {
        self.job.process_ids()
    }
}
