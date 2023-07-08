use std::{
    ffi::OsString,
    mem,
    os::windows::prelude::OsStringExt,
    path::{Path, PathBuf},
    ptr,
};

use windows_sys::Win32::{
    Foundation::{GetLastError, FALSE, HANDLE, MAX_PATH, WIN32_ERROR},
    System::{
        Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory},
        Memory::{
            VirtualQueryEx, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
            PAGE_EXECUTE_WRITECOPY, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
        },
        ProcessStatus::GetMappedFileNameW,
        Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_NATIVE, PROCESS_QUERY_INFORMATION,
            PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
        },
    },
};

use super::{
    vmmap64::{ProcessInfo, VirtualMemoryRead, VirtualMemoryWrite, VirtualQuery, VirtualQueryExt},
    Error, Pid,
};

#[derive(Debug, Clone)]
pub struct Process {
    pid: Pid,
    handle: HANDLE,
    pathname: PathBuf,
}

impl VirtualMemoryRead for Process {
    type Error = Error;

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let code =
            unsafe { ReadProcessMemory(self.handle, offset as _, buf.as_mut_ptr() as _, buf.len(), ptr::null_mut()) };
        if code == 0 {
            let error = unsafe { GetLastError() };
            return Err(Error::ReadMemory(error));
        }

        Ok(buf.len())
    }
}

impl VirtualMemoryWrite for Process {
    type Error = Error;

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), Self::Error> {
        let code =
            unsafe { WriteProcessMemory(self.handle, offset as _, buf.as_ptr() as _, buf.len(), ptr::null_mut()) };

        if code == 0 {
            let error = unsafe { GetLastError() };
            return Err(Error::WriteMemory(error));
        }

        Ok(())
    }
}

impl Process {
    pub fn open(pid: Pid) -> Result<Self, Error> {
        unsafe {
            let handle = OpenProcess(
                PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION,
                FALSE,
                pid,
            );

            if handle == 0 {
                let error = GetLastError();
                return Err(Error::OpenProcess(error));
            }

            let mut buffer = [0; MAX_PATH as _];
            let mut lpdwsize = MAX_PATH;

            let result = QueryFullProcessImageNameW(handle, PROCESS_NAME_NATIVE, buffer.as_mut_ptr(), &mut lpdwsize);
            if result == 0 {
                let error = GetLastError();
                return Err(Error::OpenProcess(error));
            }

            let pathname = PathBuf::from(OsString::from_wide(&buffer[..lpdwsize as _]));
            Ok(Self { pid, handle, pathname })
        }
    }
}

impl ProcessInfo for Process {
    fn pid(&self) -> Pid {
        self.pid
    }

    fn app_path(&self) -> &Path {
        &self.pathname
    }

    fn get_maps(&self) -> impl Iterator<Item = impl VirtualQuery + '_> {
        fn skip_last<T>(mut iter: impl Iterator<Item = T>) -> impl Iterator<Item = T> {
            let last = iter.next();
            iter.scan(last, |state, item| mem::replace(state, Some(item)))
        }
        skip_last(PageIter::new(self.handle).skip(1))
    }
}

#[derive(Debug, Clone)]
pub struct Page {
    start: u64,
    size: u64,
    flags: u32,
    pathname: Option<PathBuf>,
}

impl VirtualQueryExt for Page {
    fn path(&self) -> Option<&Path> {
        self.pathname.as_deref()
    }
}

impl VirtualQuery for Page {
    fn start(&self) -> u64 {
        self.start
    }

    fn end(&self) -> u64 {
        self.start + self.size
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn is_read(&self) -> bool {
        self.flags
            & (PAGE_EXECUTE_READ
                | PAGE_EXECUTE_READWRITE
                | PAGE_EXECUTE_WRITECOPY
                | PAGE_READONLY
                | PAGE_READWRITE
                | PAGE_WRITECOPY)
            != 0
    }

    fn is_write(&self) -> bool {
        self.flags & (PAGE_EXECUTE_READWRITE | PAGE_READWRITE) != 0
    }

    fn is_exec(&self) -> bool {
        self.flags & (PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY) != 0
    }
}

pub struct PageIter {
    handle: HANDLE,
    base: usize,
}

impl PageIter {
    pub const fn new(handle: HANDLE) -> Self {
        Self { handle, base: 0 }
    }
}

impl Iterator for PageIter {
    type Item = Page;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            let mut basic = mem::MaybeUninit::uninit();

            if VirtualQueryEx(
                self.handle,
                self.base as _,
                basic.as_mut_ptr(),
                mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            ) == 0
            {
                return None;
            }

            let pathname = get_path_name(self.handle, self.base).ok();

            let info = basic.assume_init();
            self.base = info.BaseAddress as usize + info.RegionSize;

            Some(Page {
                start: info.BaseAddress as _,
                size: info.RegionSize as _,
                flags: info.Protect,
                pathname,
            })
        }
    }
}

#[inline(always)]
unsafe fn get_path_name(handle: HANDLE, base: usize) -> Result<PathBuf, WIN32_ERROR> {
    let mut buf = [0; MAX_PATH as _];
    let result = GetMappedFileNameW(handle, base as _, buf.as_mut_ptr(), buf.len() as _);
    if result == 0 {
        return Err(GetLastError());
    }
    Ok(PathBuf::from(OsString::from_wide(&buf[..result as _])))
}
