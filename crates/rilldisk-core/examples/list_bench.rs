// SPDX-License-Identifier: AGPL-3.0-only
//! Profiling tool: how fast can each Windows directory-listing strategy walk a
//! tree, without the scanner's collector, ID assignment or rollups? Every strategy
//! is a complete recursive walk (parallel, one task per directory, reparse points
//! never followed) that only counts files, folders and bytes, so the numbers
//! isolate the cost of the system calls themselves.
//!
//! ```text
//! cargo run --release -p rilldisk-core --example list_bench -- PATH [--threads 8,24] [--runs 3] [--only NAME]
//! ```
//!
//! Output is one JSON line per (strategy, threads, run). Read-only; only counts
//! and timings are printed, never names.
#[cfg(not(windows))]
fn main() {
    eprintln!("list_bench compares Windows listing APIs; use scan_bench on this OS");
}

#[cfg(windows)]
fn main() {
    windows::main();
}

#[cfg(windows)]
mod windows {
    use std::{
        mem::offset_of,
        os::windows::ffi::OsStrExt,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::Instant,
    };
    use windows_sys::{
        Wdk::{
            Foundation::{NtClose, OBJECT_ATTRIBUTES},
            Storage::FileSystem::{
                FILE_DIRECTORY_FILE, FILE_ID_BOTH_DIR_INFORMATION, FILE_ID_EXTD_DIR_INFORMATION,
                FILE_ID_FULL_DIR_INFORMATION, FILE_INFORMATION_CLASS, FILE_OPEN,
                FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
                FileIdBothDirectoryInformation, FileIdExtdDirectoryInformation,
                FileIdFullDirectoryInformation, NtCreateFile, NtQueryDirectoryFileEx,
            },
        },
        Win32::{
            Foundation::{
                CloseHandle, HANDLE, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, UNICODE_STRING,
            },
            Storage::FileSystem::{
                CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
                FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
                FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
                FILE_SHARE_WRITE, FIND_FIRST_EX_LARGE_FETCH, FileAttributeTagInfo,
                FileIdBothDirectoryInfo, FindClose, FindExInfoBasic, FindExSearchNameMatch,
                FindFirstFileExW, FindNextFileW, GetFileInformationByHandleEx, OPEN_EXISTING,
                SYNCHRONIZE, WIN32_FIND_DATAW,
            },
            System::IO::IO_STATUS_BLOCK,
        },
    };

    thread_local! {
        static BUFFER: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Run `f` with a per-thread listing buffer of `bytes`, or a fresh zeroed
    /// allocation per directory (what the scanner did before) when `fresh`.
    fn with_buffer<R>(bytes: usize, fresh: bool, f: impl FnOnce(&mut [u64]) -> R) -> R {
        if fresh {
            return f(&mut vec![0_u64; bytes / 8]);
        }
        BUFFER.with(|cell| {
            let mut buffer = cell.borrow_mut();
            if buffer.len() < bytes / 8 {
                buffer.resize(bytes / 8, 0);
            }
            f(&mut buffer[..bytes / 8])
        })
    }

    const STATUS_NO_MORE_FILES: i32 = 0x8000_0006_u32 as i32;
    const STATUS_BUFFER_OVERFLOW: i32 = 0x8000_0005_u32 as i32;

    #[derive(Default)]
    struct Totals {
        files: AtomicU64,
        dirs: AtomicU64,
        bytes: AtomicU64,
        errors: AtomicU64,
        calls: AtomicU64,
    }

    /// Field offsets of the NT directory information classes compared here.
    #[derive(Clone, Copy)]
    struct Layout {
        class: FILE_INFORMATION_CLASS,
        name: usize,
    }

    const ID_BOTH: Layout = Layout {
        class: FileIdBothDirectoryInformation,
        name: offset_of!(FILE_ID_BOTH_DIR_INFORMATION, FileName),
    };
    const ID_FULL: Layout = Layout {
        class: FileIdFullDirectoryInformation,
        name: offset_of!(FILE_ID_FULL_DIR_INFORMATION, FileName),
    };
    const ID_EXTD: Layout = Layout {
        class: FileIdExtdDirectoryInformation,
        name: offset_of!(FILE_ID_EXTD_DIR_INFORMATION, FileName),
    };

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle was opened by this program and is closed once.
            unsafe { NtClose(self.0) };
        }
    }
    // SAFETY: kernel handles may be used from any thread.
    unsafe impl Send for Handle {}
    // SAFETY: see above; NtCreateFile relative opens only read the handle.
    unsafe impl Sync for Handle {}

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// Visit one listing buffer; returns (name, attributes, size) per record.
    fn records(bytes: &[u8], layout: Layout, mut visit: impl FnMut(&[u16], u32, u64)) {
        let mut offset = 0;
        loop {
            let rest = &bytes[offset..];
            let next = u32::from_le_bytes(rest[0..4].try_into().unwrap()) as usize;
            let size = i64::from_le_bytes(rest[40..48].try_into().unwrap()) as u64;
            let attributes = u32::from_le_bytes(rest[56..60].try_into().unwrap());
            let length = u32::from_le_bytes(rest[60..64].try_into().unwrap()) as usize;
            let name: Vec<u16> = rest[layout.name..layout.name + length]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            visit(&name, attributes, size);
            if next == 0 {
                break;
            }
            offset += next;
        }
    }

    fn is_dot(name: &[u16]) -> bool {
        name == [46] || name == [46, 46]
    }

    fn count(totals: &Totals, attributes: u32, size: u64) -> bool {
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
        if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            totals.dirs.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        totals.files.fetch_add(1, Ordering::Relaxed);
        totals.bytes.fetch_add(size, Ordering::Relaxed);
        false
    }

    // ---- Strategy 1: the current scanner (CreateFileW on the full path, an
    // attribute check, GetFileInformationByHandleEx(FileIdBothDirectoryInfo)).
    fn win32_path(
        scope: &rayon::Scope<'_>,
        totals: &Arc<Totals>,
        path: PathBuf,
        tag: bool,
        fresh: bool,
    ) {
        let name = wide(&path);
        // SAFETY: name is terminated; the handle is closed below.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            totals.errors.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if tag {
            let mut info = FILE_ATTRIBUTE_TAG_INFO {
                FileAttributes: 0,
                ReparseTag: 0,
            };
            // SAFETY: info is the documented structure for this class.
            unsafe {
                GetFileInformationByHandleEx(
                    handle,
                    FileAttributeTagInfo,
                    (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                    size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
                )
            };
        }
        with_buffer(64 << 10, fresh, |storage| {
            loop {
                totals.calls.fetch_add(1, Ordering::Relaxed);
                // SAFETY: storage is writable for its length.
                let ok = unsafe {
                    GetFileInformationByHandleEx(
                        handle,
                        FileIdBothDirectoryInfo,
                        storage.as_mut_ptr().cast(),
                        (storage.len() * 8) as u32,
                    )
                };
                if ok == 0 {
                    break;
                }
                // SAFETY: initialized storage viewed as bytes.
                let bytes = unsafe {
                    std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), storage.len() * 8)
                };
                records(bytes, ID_BOTH, |name, attributes, size| {
                    if !is_dot(name) && count(totals, attributes, size) {
                        let child = path.join(String::from_utf16_lossy(name));
                        let totals = totals.clone();
                        scope.spawn(move |s| win32_path(s, &totals, child, tag, fresh));
                    }
                });
            }
        });
        // SAFETY: opened above.
        unsafe { CloseHandle(handle) };
    }

    // ---- Strategy 2: FindFirstFileExW with FindExInfoBasic and LARGE_FETCH.
    fn find_large(scope: &rayon::Scope<'_>, totals: &Arc<Totals>, path: PathBuf) {
        let pattern = wide(&path.join("*"));
        let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
        // SAFETY: pattern is terminated; data is writable.
        let handle = unsafe {
            FindFirstFileExW(
                pattern.as_ptr(),
                FindExInfoBasic,
                (&mut data as *mut WIN32_FIND_DATAW).cast(),
                FindExSearchNameMatch,
                std::ptr::null(),
                FIND_FIRST_EX_LARGE_FETCH,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            totals.errors.fetch_add(1, Ordering::Relaxed);
            return;
        }
        loop {
            let end = data.cFileName.iter().position(|&c| c == 0).unwrap_or(260);
            let name = &data.cFileName[..end];
            let size = (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow);
            if !is_dot(name) && count(totals, data.dwFileAttributes, size) {
                let child = path.join(String::from_utf16_lossy(name));
                let totals = totals.clone();
                scope.spawn(move |s| find_large(s, &totals, child));
            }
            // SAFETY: handle from FindFirstFileExW.
            if unsafe { FindNextFileW(handle, &mut data) } == 0 {
                break;
            }
        }
        // SAFETY: handle from FindFirstFileExW.
        unsafe { FindClose(handle) };
    }

    // ---- Strategy 3: NtCreateFile + NtQueryDirectoryFileEx. Either the full NT
    // path (`\??\C:\...`), or only the name relative to the parent's open handle.
    #[derive(Clone, Copy)]
    struct Nt {
        layout: Layout,
        buffer: usize,
        relative: bool,
    }

    fn nt_open(parent: Option<&Handle>, name: &[u16]) -> Result<Handle, i32> {
        let string = UNICODE_STRING {
            Length: (name.len() * 2) as u16,
            MaximumLength: (name.len() * 2) as u16,
            Buffer: name.as_ptr().cast_mut(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent.map_or(std::ptr::null_mut(), |h| h.0),
            ObjectName: &string,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        };
        let mut handle: HANDLE = std::ptr::null_mut();
        let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        // SAFETY: all pointers reference live locals for this synchronous call.
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                FILE_LIST_DIRECTORY | SYNCHRONIZE,
                &attributes,
                &mut status_block,
                std::ptr::null(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_OPEN,
                FILE_DIRECTORY_FILE
                    | FILE_SYNCHRONOUS_IO_NONALERT
                    | FILE_OPEN_FOR_BACKUP_INTENT
                    | FILE_OPEN_REPARSE_POINT,
                std::ptr::null(),
                0,
            )
        };
        if status < 0 {
            Err(status)
        } else {
            Ok(Handle(handle))
        }
    }

    fn nt_walk(
        scope: &rayon::Scope<'_>,
        totals: &Arc<Totals>,
        parent: Option<Arc<Handle>>,
        name: Vec<u16>,
        nt: Nt,
    ) {
        let handle = match nt_open(parent.as_deref(), &name) {
            Ok(handle) => Arc::new(handle),
            Err(_) => {
                totals.errors.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        drop(parent);
        with_buffer(nt.buffer, false, |storage| {
            loop {
                let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
                totals.calls.fetch_add(1, Ordering::Relaxed);
                // SAFETY: storage is writable for its length; synchronous handle.
                let status = unsafe {
                    NtQueryDirectoryFileEx(
                        handle.0,
                        std::ptr::null_mut(),
                        None,
                        std::ptr::null(),
                        &mut status_block,
                        storage.as_mut_ptr().cast(),
                        (storage.len() * 8) as u32,
                        nt.layout.class,
                        0,
                        std::ptr::null(),
                    )
                };
                if status == STATUS_NO_MORE_FILES {
                    break;
                }
                if status < 0 && status != STATUS_BUFFER_OVERFLOW {
                    totals.errors.fetch_add(1, Ordering::Relaxed);
                    break;
                }
                // SAFETY: initialized storage viewed as bytes.
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        storage.as_ptr().cast::<u8>(),
                        status_block.Information,
                    )
                };
                if bytes.is_empty() {
                    break;
                }
                records(bytes, nt.layout, |child, attributes, size| {
                    if !is_dot(child) && count(totals, attributes, size) {
                        let totals = totals.clone();
                        if nt.relative {
                            let parent = handle.clone();
                            let child = child.to_vec();
                            scope.spawn(move |s| nt_walk(s, &totals, Some(parent), child, nt));
                        } else {
                            let mut full = name.clone();
                            full.push(u16::from(b'\\'));
                            full.extend_from_slice(child);
                            scope.spawn(move |s| nt_walk(s, &totals, None, full, nt));
                        }
                    }
                });
            }
        });
    }

    type Walker = dyn Fn(&rayon::Scope<'_>, &Arc<Totals>) + Sync;

    pub fn main() {
        let mut args = std::env::args().skip(1);
        let root = PathBuf::from(args.next().expect("usage: list_bench PATH"));
        let mut threads = vec![8_usize, 24];
        let mut runs = 3;
        let mut only: Option<String> = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--threads" => {
                    threads = args
                        .next()
                        .unwrap()
                        .split(',')
                        .map(|t| t.parse().unwrap())
                        .collect()
                }
                "--runs" => runs = args.next().unwrap().parse().unwrap(),
                "--only" => only = args.next(),
                other => panic!("unknown argument {other}"),
            }
        }
        // Verbatim (`\?\`) so paths longer than MAX_PATH open on the Win32 paths too.
        let root = root.canonicalize().unwrap();
        let plain = rilldisk_core::simplify(root.clone());
        let nt_root: Vec<u16> = r"\??\"
            .encode_utf16()
            .chain(plain.as_os_str().encode_wide())
            .collect();
        let mut strategies: Vec<(String, Box<Walker>)> = Vec::new();
        {
            let r = root.clone();
            strategies.push((
                "win32-path+tag/IdBoth/64K fresh buffer (v0.1.0)".into(),
                Box::new(move |s, t| win32_path(s, t, r.clone(), true, true)),
            ));
            let r = root.clone();
            strategies.push((
                "win32-path+tag/IdBoth/64K".into(),
                Box::new(move |s, t| win32_path(s, t, r.clone(), true, false)),
            ));
            let r = root.clone();
            strategies.push((
                "win32-path/IdBoth/64K".into(),
                Box::new(move |s, t| win32_path(s, t, r.clone(), false, false)),
            ));
            let r = root.clone();
            strategies.push((
                "FindFirstFileEx/Basic/LargeFetch".into(),
                Box::new(move |s, t| find_large(s, t, r.clone())),
            ));
        }
        for (label, layout) in [
            ("IdBoth", ID_BOTH),
            ("IdFull", ID_FULL),
            ("IdExtd", ID_EXTD),
        ] {
            for buffer in [64 << 10, 256 << 10, 1 << 20] {
                for relative in [false, true] {
                    let nt = Nt {
                        layout,
                        buffer,
                        relative,
                    };
                    let name = nt_root.clone();
                    strategies.push((
                        format!(
                            "nt-{}/{label}/{}K",
                            if relative { "relative" } else { "path" },
                            buffer >> 10
                        ),
                        Box::new(move |s, t| nt_walk(s, t, None, name.clone(), nt)),
                    ));
                }
            }
        }
        for &threads in &threads {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            for (name, walk) in &strategies {
                if only.as_ref().is_some_and(|o| !name.contains(o.as_str())) {
                    continue;
                }
                for run in 0..runs {
                    let totals = Arc::new(Totals::default());
                    let start = Instant::now();
                    pool.scope(|s| walk(s, &totals));
                    let ms = start.elapsed().as_secs_f64() * 1000.0;
                    let files = totals.files.load(Ordering::Relaxed);
                    let dirs = totals.dirs.load(Ordering::Relaxed) + 1;
                    println!(
                        "{{\"strategy\":\"{name}\",\"threads\":{threads},\"run\":{run},\"ms\":{ms:.1},\"files\":{files},\"dirs\":{dirs},\"bytes\":{},\"errors\":{},\"calls\":{},\"files_per_s\":{:.0},\"dirs_per_s\":{:.0}}}",
                        totals.bytes.load(Ordering::Relaxed),
                        totals.errors.load(Ordering::Relaxed),
                        totals.calls.load(Ordering::Relaxed),
                        files as f64 / ms * 1000.0,
                        dirs as f64 / ms * 1000.0,
                    );
                }
            }
        }
    }
}
