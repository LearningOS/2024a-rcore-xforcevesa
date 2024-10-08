//! Implementation of [`PageTableEntry`] and [`PageTable`].

use super::{frame_alloc, FrameTracker, PhysPageNum, StepByOne, VirtAddr, VirtPageNum};
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use bitflags::*;
use super::PhysAddr;

bitflags! {
    /// page table entry flags
    pub struct PTEFlags: u8 {
        const V = 1 << 0;
        const R = 1 << 1;
        const W = 1 << 2;
        const X = 1 << 3;
        const U = 1 << 4;
        const G = 1 << 5;
        const A = 1 << 6;
        const D = 1 << 7;
    }
}

#[derive(Copy, Clone)]
#[repr(C)]
/// page table entry structure
pub struct PageTableEntry {
    /// bits of page table entry
    pub bits: usize,
}

impl PageTableEntry {
    /// Create a new page table entry
    pub fn new(ppn: PhysPageNum, flags: PTEFlags) -> Self {
        PageTableEntry {
            bits: ppn.0 << 10 | flags.bits as usize,
        }
    }
    /// Create an empty page table entry
    pub fn empty() -> Self {
        PageTableEntry { bits: 0 }
    }
    /// Get the physical page number from the page table entry
    pub fn ppn(&self) -> PhysPageNum {
        (self.bits >> 10 & ((1usize << 44) - 1)).into()
    }
    /// Get the flags from the page table entry
    pub fn flags(&self) -> PTEFlags {
        PTEFlags::from_bits(self.bits as u8).unwrap()
    }
    /// The page pointered by page table entry is valid?
    pub fn is_valid(&self) -> bool {
        (self.flags() & PTEFlags::V) != PTEFlags::empty()
    }
    /// The page pointered by page table entry is readable?
    pub fn readable(&self) -> bool {
        (self.flags() & PTEFlags::R) != PTEFlags::empty()
    }
    /// The page pointered by page table entry is writable?
    pub fn writable(&self) -> bool {
        (self.flags() & PTEFlags::W) != PTEFlags::empty()
    }
    /// The page pointered by page table entry is executable?
    pub fn executable(&self) -> bool {
        (self.flags() & PTEFlags::X) != PTEFlags::empty()
    }
}

/// page table structure
pub struct PageTable {
    root_ppn: PhysPageNum,
    frames: Vec<FrameTracker>,
    mem_map: BTreeMap<VirtPageNum, FrameTracker>
}

/// Assume that it won't oom when creating/mapping.
impl PageTable {
    /// Create a new page table
    pub fn new() -> Self {
        let frame = frame_alloc().unwrap();
        PageTable {
            root_ppn: frame.ppn,
            frames: vec![frame],
            mem_map: BTreeMap::new()
        }
    }
    /// Temporarily used to get arguments from user space.
    pub fn from_token(satp: usize) -> Self {
        Self {
            root_ppn: PhysPageNum::from(satp & ((1usize << 44) - 1)),
            frames: Vec::new(),
            mem_map: BTreeMap::new()
        }
    }
    /// Find PageTableEntry by VirtPageNum, create a frame for a 4KB page table if not exist
    fn find_pte_create(&mut self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            if i == 2 {
                result = Some(pte);
                break;
            }
            if !pte.is_valid() {
                let frame = frame_alloc().unwrap();
                *pte = PageTableEntry::new(frame.ppn, PTEFlags::V);
                self.frames.push(frame);
            }
            ppn = pte.ppn();
        }
        result
    }
    /// Find PageTableEntry by VirtPageNum
    fn find_pte(&self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            if i == 2 {
                result = Some(pte);
                break;
            }
            if !pte.is_valid() {
                return None;
            }
            ppn = pte.ppn();
        }
        result
    }
    /// set the map between virtual page number and physical page number
    pub fn map(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) {
        let pte = self.find_pte_create(vpn).unwrap();
        assert!(!pte.is_valid(), "vpn {:?} is mapped before mapping", vpn);
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V);
    }
    /// remove the map between virtual page number and physical page number
    pub fn unmap(&mut self, vpn: VirtPageNum) {
        let pte = self.find_pte(vpn).unwrap();
        assert!(pte.is_valid(), "vpn {:?} is invalid before unmapping", vpn);
        *pte = PageTableEntry::empty();
    }
    /// get the page table entry from the virtual page number
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        self.find_pte(vpn).map(|pte| *pte)
    }
    /// get the token from the page table
    pub fn token(&self) -> usize {
        8usize << 60 | self.root_ppn.0
    }
    /// get the physical address from the virtual address
    pub fn translate_va(&self, va: VirtAddr) -> Option<PhysAddr> {
        self.find_pte(va.clone().floor()).map(|pte| {
            let aligned_pa: PhysAddr = pte.ppn().into();
            let offset = va.page_offset();
            let aligned_pa_usize: usize = aligned_pa.into();
            (aligned_pa_usize + offset).into()
        })
    }

    #[allow(unused)]
    /// mmap operation
    pub fn mmap(&mut self, start: usize, len: usize, port: usize) -> isize {
        // Fetch the virtual addresses
        let mut start_virt_addr = VirtPageNum(start);
        let end_virt_addr = VirtPageNum(start + len);

        println!("start {:?} {}", start_virt_addr, start);

        // Fetch the permission flag
        let mut permission_flags = PTEFlags::from_bits_truncate(port as u8);
        if port & (1 << 0) != 0 {
            permission_flags |= PTEFlags::R;
        }
        if port & (1 << 1) != 0 {
            permission_flags |= PTEFlags::W;
        }
        if port & (1 << 2) != 0 {
            permission_flags |= PTEFlags::X;
        }
        permission_flags |= PTEFlags::U;
        permission_flags |= PTEFlags::V;
        while start_virt_addr < end_virt_addr {
            // If exist, return error.
            if let Some(entry) = self.translate(start_virt_addr) {
                if entry.is_valid() {
                    println!("ERROR: ENTRY VALID {} IN {} - {}", start_virt_addr.0, start, start + len);
                    return -1;
                    // self.unmap(start_virt_addr);
                }
            }
            // Allocate frame
            if let Some(tracker) = frame_alloc() {
                self.map(start_virt_addr, tracker.ppn, permission_flags);
                self.mem_map.insert(start_virt_addr, tracker);
            } else {
                println!("ERROR: ALLOC NONE {} IN {} - {}", start_virt_addr.0, start, start + len);
                return -1;
            }
            start_virt_addr.step();
        }
        0
    }
    
    #[allow(unused)]
    /// Do mummap
    pub fn munmap(&mut self, start: usize, len: usize) -> isize {
        let mut start_virt_addr = VirtPageNum(start);
        let end_virt_addr = VirtPageNum(start + len);
        while start_virt_addr < end_virt_addr {
            if let Some(entry) = self.translate(start_virt_addr) {
                if !entry.is_valid() {
                    println!("ERROR: ENTRY INVALID {} IN {} - {}", start_virt_addr.0, start, start + len);
                    return -1;
                }
            }
            self.unmap(start_virt_addr);
            self.mem_map.remove(&start_virt_addr);
            start_virt_addr.step();
        }
        0
    }
}

/// Translate&Copy a ptr[u8] array with LENGTH len to a mutable u8 Vec through page table
pub fn translated_byte_buffer(token: usize, ptr: *const u8, len: usize) -> Vec<&'static mut [u8]> {
    let page_table = PageTable::from_token(token);
    let mut start = ptr as usize;
    let end = start + len;
    let mut v = Vec::new();
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.floor();
        let ppn = page_table.translate(vpn).unwrap().ppn();
        vpn.step();
        let mut end_va: VirtAddr = vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        if end_va.page_offset() == 0 {
            v.push(&mut ppn.get_bytes_array()[start_va.page_offset()..]);
        } else {
            v.push(&mut ppn.get_bytes_array()[start_va.page_offset()..end_va.page_offset()]);
        }
        start = end_va.into();
    }
    v
}

/// Translate a ptr[u8] array through page table and return a mutable reference of T
pub fn translated_refmut<T>(token: usize, ptr: *mut T) -> &'static mut T {
    let page_table = PageTable::from_token(token);
    let va = ptr as usize;
    page_table
        .translate_va(VirtAddr::from(va))
        .unwrap()
        .get_mut()
}

#[allow(unused)]
/// Memory map
pub fn page_table_mmap(token: usize, start: usize, len: usize, port: usize) -> isize {
    let mut page_table = PageTable::from_token(token);
    page_table.mmap(start, len, port)
}

#[allow(unused)]
/// Memory unmap
pub fn page_table_munmap(token: usize, start: usize, len: usize) -> isize {
    let mut page_table = PageTable::from_token(token);
    page_table.munmap(start, len)
}
