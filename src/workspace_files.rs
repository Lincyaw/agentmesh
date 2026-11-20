//! Descriptor-relative text operations. Never follow a symlink, including during writes.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Component, Path},
    time::{Duration, Instant},
};

pub const TEXT_LIMIT: usize = 64 * 1024;

fn components(path: &str) -> Result<Vec<String>> {
    ensure!(
        path.len() <= 4096 && !path.contains('\0'),
        "invalid relative path"
    );
    Path::new(path)
        .components()
        .map(|part| match part {
            Component::Normal(name) => Ok(name.to_str().context("path must be UTF-8")?.to_owned()),
            Component::CurDir => Ok(String::new()),
            _ => anyhow::bail!("path must be relative without parent traversal"),
        })
        .filter(|part| !matches!(part, Ok(s) if s.is_empty()))
        .collect()
}

fn open_at(parent: &File, name: &str, flags: i32) -> Result<File> {
    let name = CString::new(name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .context("cannot open workspace path (symlinks are forbidden)");
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub fn directory(root: &str, path: &str) -> Result<File> {
    ensure!(
        Path::new(root).is_absolute(),
        "workspace root must be absolute"
    );
    let mut dir = File::open("/")?;
    for name in components(root.trim_start_matches('/'))?
        .into_iter()
        .chain(components(path)?)
    {
        dir = open_at(&dir, &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
    }
    Ok(dir)
}

fn parent(root: &str, path: &str) -> Result<(File, String)> {
    let mut parts = components(path)?;
    let name = parts.pop().context("file path is required")?;
    Ok((directory(root, &parts.join("/"))?, name))
}

fn text(file: &mut File) -> Result<String> {
    ensure!(file.metadata()?.is_file(), "expected a regular file");
    let mut bytes = vec![];
    file.take((TEXT_LIMIT + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= TEXT_LIMIT, "text file exceeds 64 KiB limit");
    String::from_utf8(bytes).context("file is not UTF-8 text")
}

pub fn read(root: &str, path: &str) -> Result<Value> {
    let (parent, name) = parent(root, path)?;
    let content = text(&mut open_at(&parent, &name, libc::O_RDONLY)?)?;
    Ok(json!({"path":path,"sha256":crate::network::digest(&content),"content":content}))
}

pub fn write(root: &str, path: &str, content: &str, expected: Option<&str>) -> Result<Value> {
    ensure!(content.len() <= TEXT_LIMIT, "content exceeds 64 KiB limit");
    let (parent, name) = parent(root, path)?;
    let original = match open_at(&parent, &name, libc::O_RDONLY) {
        Ok(mut file) => Some((text(&mut file)?, file.metadata()?)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            None
        }
        Err(error) => return Err(error),
    };
    let sha = crate::network::digest(content);
    if original.as_ref().is_some_and(|(old, _)| old == content) {
        return Ok(json!({"path":path,"sha256":sha,"changed":false}));
    }
    match (&original, expected) {
        (Some((old, _)), Some(hash)) => ensure!(
            crate::network::digest(old) == hash,
            "file changed: expected_sha256 does not match; read again"
        ),
        (None, None) => (),
        _ => anyhow::bail!(
            "existing files require expected_sha256; omit it only to create a new file"
        ),
    }
    let temp = format!(".agentmesh-{}", uuid::Uuid::new_v4());
    let temp_c = CString::new(temp.as_str())?;
    let name_c = CString::new(name.as_str())?;
    let result = (|| {
        let mut file = open_at(
            &parent,
            &temp,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        )?;
        if let Some((_, metadata)) = &original {
            ensure!(
                unsafe {
                    libc::fchmod(file.as_raw_fd(), (metadata.mode() & 0o777) as libc::mode_t)
                } == 0,
                "cannot preserve file mode"
            );
        }
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        // All Agentmesh file operations are serialized by the execution manager. Local
        // writers and shell commands must still coordinate their own concurrent edits.
        let rc = unsafe {
            if original.is_none() {
                // linkat provides portable no-replace publication on Linux and macOS.
                libc::linkat(
                    parent.as_raw_fd(),
                    temp_c.as_ptr(),
                    parent.as_raw_fd(),
                    name_c.as_ptr(),
                    0,
                )
            } else {
                libc::renameat(
                    parent.as_raw_fd(),
                    temp_c.as_ptr(),
                    parent.as_raw_fd(),
                    name_c.as_ptr(),
                )
            }
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error()).context("cannot publish file");
        }
        parent.sync_all()?;
        Ok(json!({"path":path,"sha256":sha,"changed":true}))
    })();
    unsafe {
        libc::unlinkat(parent.as_raw_fd(), temp_c.as_ptr(), 0);
    }
    result
}

pub fn list(root: &str, path: &str) -> Result<Value> {
    let dir = directory(root, path)?;
    let mut entries = vec![];
    let mut truncated = false;
    for entry in names(&dir)? {
        if entries.len() >= 200 {
            truncated = true;
            break;
        }
        let name_c = CString::new(entry.as_str())?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name_c.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            continue;
        }
        let stat = unsafe { stat.assume_init() };
        let kind = match stat.st_mode & libc::S_IFMT {
            libc::S_IFREG => "file",
            libc::S_IFDIR => "directory",
            libc::S_IFLNK => "symlink",
            _ => "other",
        };
        entries.push(json!({"name":entry,"kind":kind,"bytes":stat.st_size}));
    }
    Ok(json!({"path":path,"entries":entries,"truncated":truncated}))
}

fn names(dir: &File) -> Result<Vec<String>> {
    // fdopendir owns its descriptor; open '.' to get an independent directory cursor.
    let copy = open_at(dir, ".", libc::O_RDONLY | libc::O_DIRECTORY)?;
    use std::os::fd::IntoRawFd;
    let fd = copy.into_raw_fd();
    let pointer = unsafe { libc::fdopendir(fd) };
    if pointer.is_null() {
        unsafe {
            libc::close(fd);
        }
        return Err(std::io::Error::last_os_error()).context("cannot list directory");
    }
    struct Entries(*mut libc::DIR);
    impl Drop for Entries {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let entries = Entries(pointer);
    let mut result = vec![];
    loop {
        let item = unsafe { libc::readdir(entries.0) };
        if item.is_null() {
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*item).d_name.as_ptr()) };
        let Ok(name) = name.to_str() else {
            continue;
        };
        if name != "." && name != ".." {
            result.push(name.to_owned());
        }
        ensure!(result.len() <= 10000, "directory exceeds 10000 entries");
    }
    result.sort();
    Ok(result)
}

pub fn search(root: &str, path: &str, query: &str) -> Result<Value> {
    crate::protocol::validate_text(query, "literal search query", 1024)?;
    let dir = directory(root, path)?;
    let start = Instant::now();
    drop(dir);
    let mut queue = vec![(String::new(), 0)];
    let mut matches = vec![];
    let mut scanned = 0;
    let mut visited = 0;
    let mut truncated = false;
    'walk: while let Some((prefix, depth)) = queue.pop() {
        let dir = match directory(root, &format!("{path}/{prefix}")) {
            Ok(dir) => dir,
            Err(_) => {
                truncated = true;
                continue;
            }
        };
        for name in names(&dir)? {
            visited += 1;
            if visited > 5000 || scanned >= 1000 || start.elapsed() > Duration::from_secs(2) {
                truncated = true;
                break 'walk;
            }
            let child = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let Ok(mut file) = open_at(&dir, &name, libc::O_RDONLY) else {
                continue;
            };
            let metadata = file.metadata()?;
            if metadata.is_dir() {
                if depth < 24 && name != ".git" {
                    queue.push((child, depth + 1));
                } else {
                    truncated = true;
                }
            } else if metadata.is_file() {
                scanned += 1;
                if metadata.len() > TEXT_LIMIT as u64 {
                    truncated = true;
                    continue;
                }
                let Ok(content) = text(&mut file) else {
                    continue;
                };
                for (i, line) in content.lines().enumerate() {
                    if line.contains(query) {
                        matches.push(json!({"path":child,"line":i+1,"text":line.chars().take(512).collect::<String>()}));
                        if matches.len() >= 100 {
                            truncated = true;
                            break 'walk;
                        }
                    }
                }
            }
        }
    }
    Ok(json!({"path":path,"matches":matches,"files_scanned":scanned,"truncated":truncated}))
}
