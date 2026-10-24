//! Windows Task Scheduler through its COM API (`ITaskService`): one folder's tasks, for this
//! user, with no admin rights. Locale-free, unlike `schtasks` output.

use std::io;

use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::TaskScheduler::{
    ITaskFolder, ITaskService, TASK_CREATE_OR_UPDATE, TASK_ENUM_HIDDEN,
    TASK_LOGON_INTERACTIVE_TOKEN, TaskScheduler,
};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::core::BSTR;

use super::io_error;

/// COM, initialised for this thread while it lives (if it wasn't already).
struct Com {
    owned: bool,
}

impl Com {
    fn init() -> io::Result<Self> {
        // SAFETY: initialises COM for this thread; undone in Drop when this call did it.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(Com { owned: true })
        } else if hr == RPC_E_CHANGED_MODE {
            // The thread already uses COM in another mode: use it as it is.
            Ok(Com { owned: false })
        } else {
            Err(io::Error::from_raw_os_error(hr.0))
        }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: balances the CoInitializeEx in `init`, on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// A task as Task Scheduler reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskInfo {
    pub name: String,
    pub enabled: bool,
    /// Running now.
    pub running: bool,
    /// The last run's start, as an OLE date in local time (days since 1899-12-30); `None` if
    /// it never ran.
    pub last_run: Option<f64>,
    /// The last run's result code (0 is success; Task Scheduler has its own codes for "not run
    /// yet" and the like).
    pub last_result: i32,
    /// The next run, as an OLE date in local time; `None` if none is scheduled.
    pub next_run: Option<f64>,
    /// Its definition, as Task Scheduler XML.
    pub xml: String,
}

/// One Task Scheduler folder, such as `\Replica`.
pub struct TaskFolder {
    folder: ITaskFolder,
    // Declared last, so the COM objects above are released before COM is uninitialised.
    _com: Com,
}

fn empty() -> VARIANT {
    VARIANT::default()
}

fn index(i: i32) -> VARIANT {
    let mut v = VARIANT::default();
    // SAFETY: sets the variant's type and its matching union field together.
    unsafe {
        let inner = &mut *v.Anonymous.Anonymous;
        inner.vt = VT_I4;
        inner.Anonymous.lVal = i;
    }
    v
}

fn service() -> io::Result<ITaskService> {
    // SAFETY: creates and connects the in-process Task Scheduler service as this user.
    unsafe {
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| io_error(&e))?;
        service
            .Connect(&empty(), &empty(), &empty(), &empty())
            .map_err(|e| io_error(&e))?;
        Ok(service)
    }
}

/// Splits `\A\B` into (`\A`, `B`); the parent of a top-level folder is `\`.
fn split(path: &str) -> (&str, &str) {
    match path.rfind('\\') {
        Some(0) | None => ("\\", path.trim_start_matches('\\')),
        Some(i) => (&path[..i], &path[i + 1..]),
    }
}

/// Whether a Task Scheduler error means the folder or task isn't there (file or path not
/// found), as against any other failure (access denied, the service, COM), which must be
/// reported, not taken for "nothing there".
fn is_not_found(error: &windows::core::Error) -> bool {
    const FILE_NOT_FOUND: u32 = 0x8007_0002;
    const PATH_NOT_FOUND: u32 = 0x8007_0003;
    matches!(error.code().0 as u32, FILE_NOT_FOUND | PATH_NOT_FOUND)
}

/// A lookup's result: found, not there (`None`), or an error.
fn lookup<T>(result: windows::core::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(found) => Ok(Some(found)),
        Err(e) if is_not_found(&e) => Ok(None),
        Err(e) => Err(io_error(&e)),
    }
}

impl TaskFolder {
    /// The folder at `path` (`\Replica`), made first if `create` is set. `None` if it isn't
    /// there and `create` isn't set. Any failure other than "not found" is an error.
    pub fn open(path: &str, create: bool) -> io::Result<Option<Self>> {
        let com = Com::init()?;
        let service = service()?;
        // SAFETY: COM calls on live interfaces, with BSTRs that outlive each call.
        unsafe {
            if let Some(folder) = lookup(service.GetFolder(&BSTR::from(path)))? {
                return Ok(Some(TaskFolder { folder, _com: com }));
            }
            if !create {
                return Ok(None);
            }
            let (parent, name) = split(path);
            let parent = service
                .GetFolder(&BSTR::from(parent))
                .map_err(|e| io_error(&e))?;
            let folder = parent
                .CreateFolder(&BSTR::from(name), &empty())
                .map_err(|e| io_error(&e))?;
            Ok(Some(TaskFolder { folder, _com: com }))
        }
    }

    /// Deletes the folder at `path` if it's there and empty. Returns false if it wasn't there;
    /// any other failure is an error.
    pub fn delete_folder(path: &str) -> io::Result<bool> {
        let _com = Com::init()?;
        let service = service()?;
        let (parent, name) = split(path);
        // SAFETY: COM calls on live interfaces, with BSTRs that outlive each call.
        unsafe {
            let Some(parent) = lookup(service.GetFolder(&BSTR::from(parent)))? else {
                return Ok(false);
            };
            if lookup(service.GetFolder(&BSTR::from(path)))?.is_none() {
                return Ok(false);
            }
            parent
                .DeleteFolder(&BSTR::from(name), 0)
                .map(|()| true)
                .map_err(|e| io_error(&e))
        }
    }

    /// Every task in the folder, hidden ones included.
    pub fn tasks(&self) -> io::Result<Vec<TaskInfo>> {
        // SAFETY: COM calls on live interfaces; the collection is indexed 1..=Count.
        unsafe {
            let collection = self
                .folder
                .GetTasks(TASK_ENUM_HIDDEN.0)
                .map_err(|e| io_error(&e))?;
            let count = collection.Count().map_err(|e| io_error(&e))?;
            let mut tasks = Vec::with_capacity(count.max(0) as usize);
            for i in 1..=count {
                let task = collection.get_Item(&index(i)).map_err(|e| io_error(&e))?;
                // Task Scheduler reports "never" as 1999-11-30 (or 0): anything before 2000
                // (OLE date 36526) is no date.
                let date = |d: windows::core::Result<f64>| d.ok().filter(|d| *d >= 36526.0);
                tasks.push(TaskInfo {
                    name: task.Name().map_err(|e| io_error(&e))?.to_string(),
                    enabled: task.Enabled().map(|b| b.as_bool()).unwrap_or(false),
                    running: task.State().is_ok_and(|s| {
                        s == windows::Win32::System::TaskScheduler::TASK_STATE_RUNNING
                    }),
                    last_run: date(task.LastRunTime()),
                    last_result: task.LastTaskResult().unwrap_or(0),
                    next_run: date(task.NextRunTime()),
                    xml: task.Xml().map(|x| x.to_string()).unwrap_or_default(),
                });
            }
            Ok(tasks)
        }
    }

    /// Creates the task `name` from its XML definition, or replaces it, to run as this user
    /// while they're logged on (no stored password).
    pub fn register(&self, name: &str, xml: &str) -> io::Result<()> {
        // SAFETY: a COM call on a live interface, with values that outlive it.
        unsafe {
            self.folder
                .RegisterTask(
                    &BSTR::from(name),
                    &BSTR::from(xml),
                    TASK_CREATE_OR_UPDATE.0,
                    &empty(),
                    &empty(),
                    TASK_LOGON_INTERACTIVE_TOKEN,
                    &empty(),
                )
                .map(|_| ())
                .map_err(|e| io_error(&e))
        }
    }

    /// Enables or disables the task `name`.
    pub fn set_enabled(&self, name: &str, enabled: bool) -> io::Result<()> {
        // SAFETY: COM calls on live interfaces, with a BSTR that outlives them.
        unsafe {
            let task = self
                .folder
                .GetTask(&BSTR::from(name))
                .map_err(|e| io_error(&e))?;
            task.SetEnabled(if enabled { VARIANT_TRUE } else { VARIANT_FALSE })
                .map_err(|e| io_error(&e))
        }
    }

    /// Starts the task `name` now, as Task Scheduler would.
    pub fn run(&self, name: &str) -> io::Result<()> {
        // SAFETY: COM calls on live interfaces, with values that outlive them.
        unsafe {
            let task = self
                .folder
                .GetTask(&BSTR::from(name))
                .map_err(|e| io_error(&e))?;
            task.Run(&empty()).map(|_| ()).map_err(|e| io_error(&e))
        }
    }

    /// Deletes the task `name`. Returns false if it wasn't there.
    pub fn delete(&self, name: &str) -> io::Result<bool> {
        let name = BSTR::from(name);
        // SAFETY: COM calls on a live interface, with a BSTR that outlives them.
        unsafe {
            if lookup(self.folder.GetTask(&name))?.is_none() {
                return Ok(false);
            }
            self.folder
                .DeleteTask(&name, 0)
                .map(|()| true)
                .map_err(|e| io_error(&e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{lookup, split};
    use windows::core::{Error, HRESULT};

    fn error(code: u32) -> windows::core::Result<()> {
        Err(Error::from(HRESULT(code as i32)))
    }

    /// Only "not found" means nothing is there; access denied, a stopped service or a COM
    /// failure is reported, so a delete can't claim success it didn't have.
    #[test]
    fn only_not_found_counts_as_nothing_there() {
        assert_eq!(lookup(Ok(7)).unwrap(), Some(7));
        assert_eq!(lookup(error(0x8007_0002)).unwrap(), None, "file not found");
        assert_eq!(lookup(error(0x8007_0003)).unwrap(), None, "path not found");
        for code in [0x8007_0005, 0x8007_06BA, 0x8000_4005, 0x8004_1326] {
            let e = lookup(error(code)).unwrap_err();
            assert!(e.raw_os_error().is_some(), "{code:#x}: {e}");
        }
        assert_eq!(
            lookup(error(0x8007_0005)).unwrap_err().raw_os_error(),
            Some(5),
            "access denied keeps its Win32 code"
        );
    }

    #[test]
    fn folder_paths_split_into_parent_and_name() {
        assert_eq!(split(r"\Replica"), (r"\", "Replica"));
        assert_eq!(split(r"\A\B"), (r"\A", "B"));
        assert_eq!(split("Replica"), (r"\", "Replica"));
    }
}
