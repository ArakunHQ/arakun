//! Durable file replacement for state, transcripts and caches.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Replaces `path` with `bytes` so that a crash or a power cut leaves either
/// the old file or the new one, never an empty or torn one: the bytes go to a
/// temp file next to it that is flushed to disk before it is renamed over, and
/// the directory is flushed after so the rename itself survives. Each call
/// gets its own temp file, so two saves of the same file can't interleave.
/// Creates the parent directory.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    after_queued(path);
    write_in(path, bytes, true, false)
}

/// `write` into a directory that must already be there: when it is gone,
/// this fails instead of making it again.
pub fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    after_queued(path);
    write_in(path, bytes, false, false)
}

/// `write` for a file other users of the machine must not read (agent
/// replies, tokens): on Unix the file ends up 0600, since the temp file is
/// created that way before anything goes in, and a directory it creates is
/// 0700. Elsewhere it is `write`.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    after_queued(path);
    write_in(path, bytes, true, true)
}

/// `write_private` into a directory that must already be there, like
/// `replace`.
pub fn replace_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    after_queued(path);
    write_in(path, bytes, false, true)
}

/// Creates `dir` and its missing parents. On Unix those it creates are 0700,
/// as `write_private` makes them; one already there keeps its mode.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Opens `path` to append to, for a log other users of the machine must not
/// read (an agent prints whatever it reads, keys included). `start_over`
/// empties it first. On Unix it ends up 0600, even when it was there before,
/// and a folder it creates is 0700; on Windows only this user may open it.
pub fn open_private_log(path: &Path, start_over: bool) -> io::Result<File> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        create_private_dir(dir)?;
    }
    let mut options = File::options();
    options.create(true);
    if start_over {
        options.write(true).truncate(true);
    } else {
        options.append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    // The mode above only applies to a new file; a log from before is
    // closed off here.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    crate::private_socket::owner_only(path)?;
    Ok(file)
}

/// What `path` holds, or will once the `write_soon` queued for it lands: a
/// load right after a save must not see the file from before it.
pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    match queued_bytes(path) {
        Some(bytes) => Ok(bytes.to_vec()),
        None => std::fs::read(path),
    }
}

/// `write` with `v` as JSON.
pub fn save_json(path: &Path, v: &impl serde::Serialize) -> io::Result<()> {
    write(path, &serde_json::to_vec(v).map_err(io::Error::other)?)
}

/// `save_json` for a cache: the next load makes it again, so a failure is
/// only logged.
pub fn cache_json(path: &Path, v: &impl serde::Serialize) {
    if let Err(e) = save_json(path, v) {
        eprintln!("arakun: could not save {}: {e}", path.display());
    }
}

/// The JSON in `path`; `None` when there is none, or it doesn't parse (a
/// cache from another version). For files whose loss costs nothing; state
/// that must not be lost goes through `stored::load`, which keeps a file it
/// can't read.
pub fn load_json<T: serde::de::DeserializeOwned>(path: impl AsRef<Path>) -> Option<T> {
    serde_json::from_slice(&read(path.as_ref()).ok()?).ok()
}

fn write_in(path: &Path, bytes: &[u8], make_dir: bool, private: bool) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if make_dir {
        if private {
            create_private_dir(dir)?;
        } else {
            std::fs::create_dir_all(dir)?;
        }
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other(format!("{} has no file name", path.display())))?;
    let (tmp, mut file) = create_temp(dir, name, private, || SEQ.fetch_add(1, Ordering::Relaxed))?;
    let replaced = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            std::fs::rename(&tmp, path)
        });
    // The temp file is ours here: it was created by this call, so removing it
    // can't delete a file someone else put there.
    if replaced.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    replaced?;
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// `write` on a background thread, for saves made on the UI thread, where the
/// disk flushes would stall the window (a settings switch, a field saved on
/// every key). One thread writes them in the order they came, so the file ends
/// with the last one. A failure is logged after `what`.
pub fn write_soon(path: PathBuf, bytes: Vec<u8>, what: &'static str) {
    queue(Save {
        path,
        bytes: bytes.into(),
        what,
        private: false,
        done: None,
        seq: 0,
    });
}

/// `write_private` on the same thread and in the same order as `write_soon`.
/// What comes back says whether the file holds these bytes, or a later save
/// that replaced them before they were written: false when that failed.
pub fn write_private_soon(
    path: PathBuf,
    bytes: Vec<u8>,
    what: &'static str,
) -> futures::channel::oneshot::Receiver<bool> {
    let (done, saved) = futures::channel::oneshot::channel();
    queue(Save {
        path,
        bytes: bytes.into(),
        what,
        private: true,
        done: Some(vec![done]),
        seq: 0,
    });
    saved
}

/// The saves queued and not yet on disk, by file: the last one for each, and
/// the number it was queued under.
type Queued = HashMap<PathBuf, (u64, Arc<[u8]>)>;

fn queued() -> &'static Mutex<Queued> {
    static QUEUED: std::sync::OnceLock<Mutex<Queued>> = std::sync::OnceLock::new();
    QUEUED.get_or_init(Default::default)
}

fn queued_bytes(path: &Path) -> Option<Arc<[u8]>> {
    let queued = queued().lock().unwrap_or_else(|e| e.into_inner());
    queued.get(path).map(|(_, bytes)| bytes.clone())
}

/// Lets the saves queued for `path` land first, so an older one can't land
/// over a write made on the spot after it.
fn after_queued(path: &Path) {
    if queued_bytes(path).is_some() {
        flush();
    }
}

fn queue(mut save: Save) {
    {
        let mut queued = queued().lock().unwrap_or_else(|e| e.into_inner());
        save.seq = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
        queued.insert(save.path.clone(), (save.seq, save.bytes.clone()));
    }
    if let Err(std::sync::mpsc::SendError(Soon::Write(save))) = soon().send(Soon::Write(save)) {
        save.write();
    }
}

/// Waits until every `write_soon` so far is on disk (before quitting).
pub fn flush() {
    let (done, wait) = std::sync::mpsc::channel();
    if soon().send(Soon::Flush(done)).is_ok() {
        let _ = wait.recv();
    }
}

struct Save {
    path: PathBuf,
    bytes: Arc<[u8]>,
    what: &'static str,
    private: bool,
    /// Its number in `queued`, which it leaves once written unless a later
    /// save of the same file took its place there.
    seq: u64,
    /// Told whether it was written. Those of the saves it replaced in a batch
    /// are told too: what they waited for is now this one.
    done: Option<Vec<futures::channel::oneshot::Sender<bool>>>,
}

impl Save {
    fn write(self) {
        // Not `write`: that waits for the queue, which is this thread.
        let written = write_in(&self.path, &self.bytes, true, self.private);
        {
            let mut queued = queued().lock().unwrap_or_else(|e| e.into_inner());
            if queued
                .get(&self.path)
                .is_some_and(|(seq, _)| *seq == self.seq)
            {
                queued.remove(&self.path);
            }
        }
        if let Err(e) = &written {
            eprintln!("arakun: {}: {e}", self.what);
        }
        for done in self.done.into_iter().flatten() {
            let _ = done.send(written.is_ok());
        }
    }
}

enum Soon {
    Write(Save),
    Flush(std::sync::mpsc::Sender<()>),
}

fn soon() -> &'static std::sync::mpsc::Sender<Soon> {
    static WRITER: std::sync::OnceLock<std::sync::mpsc::Sender<Soon>> = std::sync::OnceLock::new();
    WRITER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Soon>();
        let spawned = std::thread::Builder::new()
            .name("durable-writer".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    let mut batch = vec![first];
                    batch.extend(rx.try_iter());
                    write_batch(batch);
                }
            });
        if let Err(e) = spawned {
            // The receiver is gone with the closure: sends fail, and
            // `write_soon` writes on the spot.
            eprintln!("arakun: could not start the file writer: {e}");
        }
        tx
    })
}

/// Writes only the last of the queued saves to each file, since each one
/// replaces the whole file: a field saved on every key leaves no backlog for
/// the quit to wait on. Flushes are answered after, when what they waited for
/// (or something newer) is on disk.
fn write_batch(batch: Vec<Soon>) {
    let mut writes: Vec<Save> = Vec::new();
    let mut flushes = Vec::new();
    for save in batch {
        match save {
            Soon::Write(mut save) => {
                if let Some(i) = writes.iter().position(|w| w.path == save.path) {
                    let replaced = writes.remove(i);
                    if let Some(waiting) = replaced.done {
                        save.done.get_or_insert_default().extend(waiting);
                    }
                }
                writes.push(save);
            }
            Soon::Flush(done) => flushes.push(done),
        }
    }
    for save in writes {
        save.write();
    }
    for done in flushes {
        let _ = done.send(());
    }
}

/// How many taken temp names `create_temp` skips before it gives up.
const TEMP_TRIES: u32 = 64;

/// Creates a new temp file for `name` in `dir`. The file is created
/// exclusively: a file already at that name (left by a crash, or put there by
/// another user of a shared directory to read what we write) is never opened,
/// and the next name is tried. On Unix a private file is 0600 from the start,
/// which a pre-existing file would not be.
fn create_temp(
    dir: &Path,
    name: &std::ffi::OsStr,
    private: bool,
    mut next: impl FnMut() -> u64,
) -> io::Result<(std::path::PathBuf, File)> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    for _ in 0..TEMP_TRIES {
        let mut tmp = OsString::from(".");
        tmp.push(name);
        tmp.push(format!(".{}-{}.tmp", std::process::id(), next()));
        let tmp = dir.join(tmp);
        match opts.open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    // Leave `name` out: some callers name files after ids (a session is
    // `<session-id>.json`) and print this error to the log.
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("no free temp file name in {}", dir.display()),
    ))
}

#[cfg(test)]
mod tests {
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("arakun-durable-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn entries(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn replaces_the_file_and_leaves_nothing_behind() {
        let dir = scratch("ok");
        let path = dir.join("deep").join("state.json");
        super::write(&path, b"one").unwrap();
        super::write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(entries(&dir.join("deep")), ["state.json"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_and_its_new_directory_are_the_owners_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = scratch("private");
        let path = dir.join("deep").join("notices.json");
        // Replacing a file that was readable by all must not keep its mode.
        std::fs::create_dir_all(dir.join("deep")).unwrap();
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        super::write_private(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(mode(&path), 0o600);
        let fresh = dir.join("fresh").join("notices.json");
        super::write_private(&fresh, b"x").unwrap();
        assert_eq!(mode(&fresh), 0o600);
        assert_eq!(mode(&dir.join("fresh")), 0o700);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn replacing_privately_needs_the_directory_and_leaves_a_private_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = scratch("replace-private");
        let path = dir.join("session").join("meta.json");
        assert!(super::replace_private(&path, b"x").is_err());
        super::create_private_dir(&dir.join("session")).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("session")), 0o700);
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        super::replace_private(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(mode(&path), 0o600);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_temp_file_someone_else_made_is_left_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = scratch("planted");
        std::fs::create_dir_all(&dir).unwrap();
        // Another user plants a file readable by all at the first name.
        let planted = dir.join(format!(".notices.json.{}-0.tmp", std::process::id()));
        std::fs::write(&planted, b"planted").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o666)).unwrap();
        let mut seq = 0..;
        let (tmp, _file) =
            super::create_temp(&dir, std::ffi::OsStr::new("notices.json"), true, || {
                seq.next().unwrap()
            })
            .unwrap();
        assert_ne!(tmp, planted);
        assert_eq!(mode(&tmp), 0o600);
        assert_eq!(std::fs::read(&planted).unwrap(), b"planted");
        assert_eq!(mode(&planted), 0o666);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn gives_up_when_every_temp_name_is_taken() {
        let dir = scratch("taken-names");
        std::fs::create_dir_all(&dir).unwrap();
        let name = "session-4f9c2e.json";
        let planted = dir.join(format!(".{name}.{}-7.tmp", std::process::id()));
        std::fs::write(&planted, b"x").unwrap();
        let err = super::create_temp(&dir, std::ffi::OsStr::new(name), false, || 7).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(!err.to_string().contains("4f9c2e"), "{err}");
        assert_eq!(std::fs::read(&planted).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn writes_soon_in_order_and_flushes() {
        let dir = scratch("soon");
        let path = dir.join("settle-after-merge");
        for n in 0..50 {
            super::write_soon(path.clone(), format!("{n}").into_bytes(), "test");
        }
        super::flush();
        assert_eq!(std::fs::read(&path).unwrap(), b"49");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_read_right_after_a_save_soon_sees_it() {
        let dir = scratch("read-soon");
        let path = dir.join("pins.json");
        super::write(&path, b"old").unwrap();
        for n in 0..20 {
            let bytes = format!("{n}").into_bytes();
            super::write_soon(path.clone(), bytes.clone(), "test");
            assert_eq!(super::read(&path).unwrap(), bytes);
        }
        super::flush();
        assert_eq!(super::read(&path).unwrap(), b"19");
        assert!(
            super::queued_bytes(&path).is_none(),
            "nothing is left queued"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_write_on_the_spot_lands_after_the_saves_queued_before_it() {
        let dir = scratch("spot-after-soon");
        let path = dir.join("phone.json");
        for n in 0..20 {
            super::write_soon(path.clone(), format!("{n}").into_bytes(), "test");
        }
        super::write_private(&path, b"now").unwrap();
        super::flush();
        assert_eq!(std::fs::read(&path).unwrap(), b"now");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn save(path: &std::path::Path, bytes: &[u8]) -> super::Soon {
        super::Soon::Write(super::Save {
            path: path.to_path_buf(),
            bytes: bytes.into(),
            what: "test",
            private: false,
            done: None,
            seq: 0,
        })
    }

    #[test]
    fn a_batch_writes_each_file_once_with_its_last_save() {
        let dir = scratch("batch");
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a"), dir.join("b"));
        let (done, wait) = std::sync::mpsc::channel();
        super::write_batch(vec![
            save(&a, b"a1"),
            save(&b, b"b1"),
            super::Soon::Flush(done),
            save(&a, b"a2"),
        ]);
        assert!(wait.try_recv().is_ok(), "the flush is answered");
        assert_eq!(std::fs::read(&a).unwrap(), b"a2");
        assert_eq!(std::fs::read(&b).unwrap(), b"b1");
        assert_eq!(entries(&dir), ["a", "b"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_save_replaced_in_a_batch_hears_how_the_last_one_went() {
        let dir = scratch("replaced");
        let taken = dir.join("taken");
        std::fs::create_dir_all(taken.join("inside")).unwrap();
        let waiting = |path: &std::path::Path, bytes: &[u8]| {
            let (done, heard) = futures::channel::oneshot::channel();
            let mut soon = save(path, bytes);
            if let super::Soon::Write(s) = &mut soon {
                s.done = Some(vec![done]);
            }
            (soon, heard)
        };
        let (first, first_heard) = waiting(&taken, b"1");
        let (last, last_heard) = waiting(&taken, b"2");
        super::write_batch(vec![first, last]);
        // A directory is in the way: neither is on disk, and both hear so.
        assert_eq!(futures::executor::block_on(first_heard), Ok(false));
        assert_eq!(futures::executor::block_on(last_heard), Ok(false));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_private_log_is_only_for_this_user_even_from_before() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("private-log");
        let path = dir.join("agents").join("claude.log");
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        super::open_private_log(&path, false).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir.join("agents")), 0o700);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&path, b"old\n").unwrap();
        super::open_private_log(&path, false).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"old\n");
        super::open_private_log(&path, true).unwrap();
        assert!(std::fs::read(&path).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_private_soon_and_says_so() {
        let dir = scratch("private-soon");
        let path = dir.join("servers.json");
        let saved = super::write_private_soon(path.clone(), b"[]".to_vec(), "test");
        assert_eq!(futures::executor::block_on(saved), Ok(true));
        assert_eq!(std::fs::read(&path).unwrap(), b"[]");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn json_round_trips_and_a_bad_file_reads_as_none() {
        let dir = scratch("json");
        let path = dir.join("cache").join("seats.json");
        assert_eq!(super::load_json::<Vec<u32>>(&path), None);
        super::save_json(&path, &vec![3, 1]).unwrap();
        assert_eq!(super::load_json(&path), Some(vec![3, 1]));
        std::fs::write(&path, b"{torn").unwrap();
        assert_eq!(super::load_json::<Vec<u32>>(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_write_keeps_no_temp_file() {
        let dir = scratch("fail");
        let path = dir.join("taken");
        std::fs::create_dir_all(path.join("inside")).unwrap();
        assert!(super::write(&path, b"x").is_err());
        assert_eq!(entries(&dir), ["taken"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
