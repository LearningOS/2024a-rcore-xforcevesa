//! `Arc<Inode>` -> `OSInodeInner`: In order to open files concurrently
//! we need to wrap `Inode` into `Arc`,but `Mutex` in `Inode` prevents
//! file systems from being accessed simultaneously
//!
//! `UPSafeCell<OSInodeInner>` -> `OSInode`: for static `ROOT_INODE`,we
//! need to wrap `OSInodeInner` into `UPSafeCell`
use super::{File, Stat, StatMode};
use crate::mm::UserBuffer;
use crate::sync::UPSafeCell;
use crate::drivers::BLOCK_DEVICE;
use alloc::{collections::BTreeMap, sync::Arc};
use alloc::vec::Vec;
use bitflags::*;
use easy_fs::{EasyFileSystem, Inode};
use lazy_static::*;

/// inode in memory
/// A wrapper around a filesystem inode
/// to implement File trait atop
pub struct OSInode {
    readable: bool,
    writable: bool,
    inner: UPSafeCell<OSInodeInner>,
}
/// The OS inode inner in 'UPSafeCell'
pub struct OSInodeInner {
    offset: usize,
    inode: Arc<Inode>,
}

impl OSInode {
    /// create a new inode in memory
    pub fn new(readable: bool, writable: bool, inode: Arc<Inode>) -> Self {
        Self {
            readable,
            writable,
            inner: unsafe { UPSafeCell::new(OSInodeInner { offset: 0, inode }) },
        }
    }
    /// read all data from the inode
    pub fn read_all(&self) -> Vec<u8> {
        let mut inner = self.inner.exclusive_access();
        let mut buffer = [0u8; 512];
        let mut v: Vec<u8> = Vec::new();
        loop {
            let len = inner.inode.read_at(inner.offset, &mut buffer);
            if len == 0 {
                break;
            }
            inner.offset += len;
            v.extend_from_slice(&buffer[..len]);
        }
        v
    }

    /// check if the inode is flag deleted
    pub fn is_deleted(&self, name: &str) -> bool {
        self.inner.exclusive_access().inode.is_removed(name)
    }

    /// check if the inode is a link
    pub fn is_link(&self) -> bool {
        self.inner.exclusive_access().inode.is_link()
    }
}

lazy_static! {
    pub static ref ROOT_INODE: Arc<Inode> = {
        let efs = EasyFileSystem::open(BLOCK_DEVICE.clone());
        Arc::new(EasyFileSystem::root_inode(&efs))
    };
    pub static ref INODE_LINK_MAP: UPSafeCell<BTreeMap<u32, u32>> = {
        let map = BTreeMap::new();
        unsafe { UPSafeCell::new(map) }
    };
}

/// List all apps in the root directory
pub fn list_apps() {
    println!("/**** APPS ****");
    for app in ROOT_INODE.ls() {
        println!("{}", app);
    }
    println!("**************/");
}

bitflags! {
    ///  The flags argument to the open() system call is constructed by ORing together zero or more of the following values:
    pub struct OpenFlags: u32 {
        /// readyonly
        const RDONLY = 0;
        /// writeonly
        const WRONLY = 1 << 0;
        /// read and write
        const RDWR = 1 << 1;
        /// create new file
        const CREATE = 1 << 9;
        /// truncate file size to 0
        const TRUNC = 1 << 10;
    }
}

impl OpenFlags {
    /// Do not check validity for simplicity
    /// Return (readable, writable)
    pub fn read_write(&self) -> (bool, bool) {
        if self.is_empty() {
            (true, false)
        } else if self.contains(Self::WRONLY) {
            (false, true)
        } else {
            (true, true)
        }
    }
}

/// Open a file
pub fn open_file(name: &str, flags: OpenFlags) -> Option<Arc<OSInode>> {
    let (readable, writable) = flags.read_write();
    if flags.contains(OpenFlags::CREATE) {
        if let Some(inode) = ROOT_INODE.find(name) {
            // clear size
            inode.clear();
            Some(Arc::new(OSInode::new(readable, writable, inode)))
        } else {
            // create file
            ROOT_INODE
                .create(name)
                .map(|inode| Arc::new(OSInode::new(readable, writable, inode)))
        }
    } else {
        ROOT_INODE.find(name).map(|inode| {
            if flags.contains(OpenFlags::TRUNC) {
                inode.clear();
            }
            Arc::new(OSInode::new(readable, writable, inode))
        })
    }
}

impl File for OSInode {
    fn readable(&self) -> bool {
        self.readable
    }
    fn writable(&self) -> bool {
        self.writable
    }
    fn read(&self, mut buf: UserBuffer) -> usize {
        let mut inner = self.inner.exclusive_access();
        let mut total_read_size = 0usize;
        for slice in buf.buffers.iter_mut() {
            let read_size = inner.inode.read_at(inner.offset, *slice);
            if read_size == 0 {
                break;
            }
            inner.offset += read_size;
            total_read_size += read_size;
        }
        total_read_size
    }
    fn write(&self, buf: UserBuffer) -> usize {
        let mut inner = self.inner.exclusive_access();
        let mut total_write_size = 0usize;
        for slice in buf.buffers.iter() {
            let write_size = inner.inode.write_at(inner.offset, *slice);
            assert_eq!(write_size, slice.len());
            inner.offset += write_size;
            total_write_size += write_size;
        }
        total_write_size
    }
    fn stat(&self) -> Option<Stat> {
        let inner = self.inner.exclusive_access();

        Some(Stat {
            dev: 0,
            ino: inner.inode.get_inode().into(),
            mode: {
                match inner.inode.is_dir() {
                    true => StatMode::DIR,
                    false => StatMode::FILE,
                }
            },
            nlink: {
                let map = INODE_LINK_MAP.exclusive_access();
                let count = map
                    .get((&inner.inode.get_inode()).into())
                    .cloned()
                    .unwrap_or(1);

                count
            },
            pad: [0; 7],
        })
    }
}

/// link two files
pub fn link_file(old_name: &str, new_name: &str) -> isize {
    if old_name == new_name {
        return -1;
    }

    if let Some(old_inode) = ROOT_INODE.find(old_name) {
        if let Some(new_inode) = ROOT_INODE.create_link(new_name, old_inode.get_inode()) {
            // increments link count
            let mut inner = INODE_LINK_MAP.exclusive_access();
            let old_count = inner.get(&new_inode.get_inode().into()).cloned().unwrap_or(1);
            inner.insert(old_inode.get_inode().into(), old_count + 1);
            0
        } else {
            -1
        }
    } else {
        -1
    }
}

/// unlink a file
pub fn unlink_file(file_name: &str) -> isize {
    if let Some(inode) = ROOT_INODE.find(file_name) {
        // flag in remove
        inode.unlink(file_name);

        // decrease link count
        let mut inner = INODE_LINK_MAP.exclusive_access();
        let old_count = inner.get(&inode.get_inode().into()).cloned().unwrap_or(1);
        inner.insert(inode.get_inode().into(), old_count - 1);

        if old_count == 0 {
            inner.remove(&inode.get_inode().into());
        }

        0
    } else {
        -1
    }
}