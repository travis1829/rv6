/// Driver for qemu's virtio disk device.
/// Uses qemu's mmio interface to virtio.
/// qemu presents a "legacy" virtio interface.
///
/// qemu ... -drive file=fs.img,if=none,format=raw,id=x0 -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0
use core::array::IntoIter;
use core::marker::PhantomPinned;
use core::mem;
use core::pin::Pin;
use core::ptr;
use core::sync::atomic::{fence, Ordering};

use arrayvec::ArrayVec;
use pin_project::pin_project;

use super::{
    MmioRegs, VirtIoFeatures, VirtIoStatus, VirtqAvail, VirtqDesc, VirtqDescFlags, VirtqUsed, NUM,
    VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT,
};
use crate::{
    bio::Buf,
    kernel::kernel,
    param::BSIZE,
    riscv::{PGSHIFT, PGSIZE},
    sleepablelock::{Sleepablelock, SleepablelockGuard},
};

// It must be page-aligned.
// It needs repr(C) because it is read by device.
// https://github.com/kaist-cp/rv6/issues/52
#[repr(C, align(4096))]
#[pin_project]
pub struct VirtIoDisk {
    /// The first region is a set (not a ring) of DMA descriptors, with which
    /// the driver tells the device where to read and write individual disk
    /// operations. There are NUM descriptors. Most commands consist of a
    /// "chain" (a linked list) of a couple of these descriptors.
    desc: [VirtqDesc; NUM],

    /// The next is a ring in which the driver writes descriptor numbers that
    /// the driver would like the device to process. It only includes the head
    /// descriptor of each chain. The ring has NUM elements.
    avail: VirtqAvail,

    /// Finally a ring in which the device writes descriptor numbers that the
    /// device has finished processing (just the head of each chain). There are
    /// NUM used ring entries.
    used: VirtqUsed,

    #[pin]
    info: DiskInfo,
}

// It must be page-aligned because a virtqueue (desc + avail + used) occupies
// two or more physically-contiguous pages.
#[repr(align(4096))]
#[pin_project]
struct DiskInfo {
    /// is a descriptor free?
    /// TODO(https://github.com/kaist-cp/rv6/issues/368): can be implemented with bitmap
    free: [bool; NUM],

    /// we've looked this far in used.
    used_idx: u16,

    /// Track info about in-flight operations, for use when completion
    /// interrupt arrives. Indexed by first descriptor index of chain.
    #[pin]
    inflight: [InflightInfo; NUM],

    /// Disk command headers. One-for-one with descriptors, for convenience.
    #[pin]
    ops: [VirtIoBlockOutHeader; NUM],
}

/// # Safety
///
/// `b` refers to a valid `Buf` unless it is null.
#[pin_project]
#[derive(Copy, Clone)]
struct InflightInfo {
    b: *mut Buf<'static>,
    #[pin]
    status: bool,
    #[pin]
    _marker: PhantomPinned,
}

/// The format of the first descriptor in a disk request. To be followed by two
/// more descriptors containing the block, and a one-byte status.
// It needs repr(C) because it is read by device.
// https://github.com/kaist-cp/rv6/issues/52
#[repr(C)]
#[derive(Copy, Clone)]
struct VirtIoBlockOutHeader {
    typ: u32,
    reserved: u32,
    sector: usize,
    _marker: PhantomPinned,
}

impl VirtIoDisk {
    pub const fn zero() -> Self {
        Self {
            desc: [VirtqDesc::zero(); NUM],
            avail: VirtqAvail::zero(),
            used: VirtqUsed::zero(),
            info: DiskInfo::zero(),
        }
    }
}

impl DiskInfo {
    const fn zero() -> Self {
        Self {
            free: [true; NUM],
            used_idx: 0,
            inflight: [InflightInfo::zero(); NUM],
            ops: [VirtIoBlockOutHeader::zero(); NUM],
        }
    }

    /// Assigns a new `VirtIoBlockOutHeader` at index `index` after dropping the original one.
    /// Then, returns an immutable reference to it.
    fn set_op(
        self: Pin<&mut Self>,
        index: usize,
        op: VirtIoBlockOutHeader,
    ) -> &VirtIoBlockOutHeader {
        // Safe since we drop the element at `index` before assigning.
        let this = unsafe { self.get_unchecked_mut() };
        this.ops[index] = op;
        &this.ops[index]
    }

    /// Assigns a new `InflightInfo` at index `index` after dropping the original one.
    /// Then, returns an immutable reference to it.
    fn set_inflight(self: Pin<&mut Self>, index: usize, inflight: InflightInfo) -> &InflightInfo {
        // Safe since we drop the element at `index` before assigning.
        let this = unsafe { self.get_unchecked_mut() };
        this.inflight[index] = inflight;
        &this.inflight[index]
    }

    /// Drops the `InflightInfo` at index `index`, and fills it with `InflightInfo::zero()`.
    fn clear_inflight(self: Pin<&mut Self>, index: usize) {
        let _ = self.set_inflight(index, InflightInfo::zero());
    }
}

impl InflightInfo {
    const fn zero() -> Self {
        Self {
            b: ptr::null_mut(),
            status: false,
            _marker: PhantomPinned,
        }
    }

    fn new(b: &mut Buf<'static>) -> Self {
        Self {
            // It does not break the invariant because b is &mut Buf, which refers
            // to a valid Buf.
            b,
            // device writes 0 on success
            status: true,
            _marker: PhantomPinned,
        }
    }
}

impl VirtIoBlockOutHeader {
    const fn zero() -> Self {
        Self {
            typ: 0,
            reserved: 0,
            sector: 0,
            _marker: PhantomPinned,
        }
    }

    fn new(write: bool, sector: usize) -> Self {
        let typ = if write {
            VIRTIO_BLK_T_OUT
        } else {
            VIRTIO_BLK_T_IN
        };

        Self {
            typ,
            reserved: 0,
            sector,
            _marker: PhantomPinned,
        }
    }
}

/// A descriptor allocated by driver.
#[derive(Debug)]
struct Descriptor {
    idx: usize,
}

impl Descriptor {
    fn new(idx: usize) -> Self {
        Self { idx }
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // HACK(@efenniht): we really need linear type here:
        // https://github.com/rust-lang/rfcs/issues/814
        panic!("Descriptor must never drop. Use Disk::free instead.");
    }
}

impl Sleepablelock<VirtIoDisk> {
    /// Return a locked Buf with the `latest` contents of the indicated block.
    /// If buf.valid is true, we don't need to access Disk.
    pub fn read(&self, dev: u32, blockno: u32) -> Buf<'static> {
        let mut buf = unsafe { kernel().get_bcache() }
            .get_buf(dev, blockno)
            .lock();
        if !buf.deref_inner().valid {
            VirtIoDisk::rw(&mut self.lock(), &mut buf, false);
            buf.deref_inner_mut().valid = true;
        }
        buf
    }

    pub fn write(&self, b: &mut Buf<'static>) {
        VirtIoDisk::rw(&mut self.lock(), b, true)
    }
}

impl VirtIoDisk {
    pub fn init(&self) {
        let mut status: VirtIoStatus = VirtIoStatus::empty();

        // MMIO registers are located below KERNBASE, while kernel text and data
        // are located above KERNBASE, so we can safely read/write MMIO registers.
        MmioRegs::check_virtio_disk();
        status.insert(VirtIoStatus::ACKNOWLEDGE);
        MmioRegs::set_status(&status);
        status.insert(VirtIoStatus::DRIVER);
        MmioRegs::set_status(&status);

        // Negotiate features
        let features = MmioRegs::get_features()
            - (VirtIoFeatures::BLK_F_RO
                | VirtIoFeatures::BLK_F_SCSI
                | VirtIoFeatures::BLK_F_CONFIG_WCE
                | VirtIoFeatures::BLK_F_MQ
                | VirtIoFeatures::F_ANY_LAYOUT
                | VirtIoFeatures::RING_F_EVENT_IDX
                | VirtIoFeatures::RING_F_INDIRECT_DESC);

        MmioRegs::set_features(&features);

        // Tell device that feature negotiation is complete.
        status.insert(VirtIoStatus::FEATURES_OK);
        MmioRegs::set_status(&status);

        // Tell device we're completely ready.
        status.insert(VirtIoStatus::DRIVER_OK);
        MmioRegs::set_status(&status);
        // Safe since page size is `PGSIZE`.
        unsafe {
            MmioRegs::set_pg_size(PGSIZE as _);
        }

        // Initialize queue 0.
        unsafe {
            MmioRegs::select_and_init_queue(
                0,
                NUM as _,
                (self.desc.as_ptr() as usize >> PGSHIFT) as _,
            );
        }

        // plic.rs and trap.rs arrange for interrupts from VIRTIO0_IRQ.
    }

    // This method reads and writes disk by reading and writing MMIO registers.
    // By the construction of the kernel page table in KernelMemory::new, the
    // virtual addresses of the MMIO registers are mapped to the proper physical
    // addresses. Therefore, this method is safe.
    fn rw(guard: &mut SleepablelockGuard<'_, Self>, b: &mut Buf<'static>, write: bool) {
        let sector: usize = (*b).blockno as usize * (BSIZE / 512);

        // The spec's Section 5.2 says that legacy block operations use
        // three descriptors: one for type/reserved/sector, one for the
        // data, one for a 1-byte status result.

        // Allocate the three descriptors.
        let desc = loop {
            match guard.get_pin_mut().alloc_three_descriptors() {
                Some(idx) => break idx,
                // We do not need wakeup for the None case:
                // * alloc_three_descriptors can be executed by one thread at
                //   once. Thus, we do not need to consider interleaving of
                //   alloc_three_descriptors.
                // * If alloc_three_descriptors fails, it frees only the
                //   descriptors that it created. It does not increase the
                //   number of free descriptors. Therefore, sleeping threads
                //   do not need to wake up, as alloc_three_descriptors will
                //   still fail.
                None => guard.sleep(),
            }
        };

        let mut this = guard.get_pin_mut().project();

        // Format the three descriptors.
        // qemu's virtio-blk.c reads them.

        // 1. Set the first descriptor.
        let buf0 = this
            .info
            .as_mut()
            .set_op(desc[0].idx, VirtIoBlockOutHeader::new(write, sector));

        this.desc[desc[0].idx] = VirtqDesc {
            addr: buf0 as *const _ as _,
            len: mem::size_of::<VirtIoBlockOutHeader>() as _,
            flags: VirtqDescFlags::NEXT,
            next: desc[1].idx as _,
        };

        // 2. Set the second descriptor.
        // Device reads/writes b->data
        this.desc[desc[1].idx] = VirtqDesc {
            addr: b.deref_inner().data.as_ptr() as _,
            len: BSIZE as _,
            flags: if write {
                VirtqDescFlags::NEXT
            } else {
                VirtqDescFlags::NEXT | VirtqDescFlags::WRITE
            },
            next: desc[2].idx as _,
        };

        // 3. Set the third descriptor.
        // Record struct Buf for virtio_disk_intr().
        b.deref_inner_mut().disk = true;

        // device writes 0 on success
        let info = this
            .info
            .as_mut()
            .set_inflight(desc[0].idx, InflightInfo::new(b));

        // Device writes the status
        this.desc[desc[2].idx] = VirtqDesc {
            addr: &info.status as *const _ as _,
            len: 1,
            flags: VirtqDescFlags::WRITE,
            next: 0,
        };

        // Tell the device the first index in our chain of descriptors.
        let ring_idx = this.avail.idx as usize % NUM;
        this.avail.ring[ring_idx] = desc[0].idx as _;

        fence(Ordering::SeqCst);

        // Tell the device another avail ring entry is available.
        this.avail.idx += 1;

        fence(Ordering::SeqCst);

        // Safe since the all three descriptors' fields are well set.
        // Value is queue number.
        unsafe {
            MmioRegs::notify_queue(0);
        }

        // Wait for virtio_disk_intr() to say request has finished.
        while b.deref_inner().disk {
            (*b).vdisk_request_waitchannel
                .sleep(guard, &kernel().current_proc().expect("No current proc"));
        }

        // As it assigns null, the invariant of inflight is maintained even if
        // b: &mut Buf becomes invalid after this method returns.
        guard
            .get_pin_mut()
            .project()
            .info
            .clear_inflight(desc[0].idx);
        IntoIter::new(desc).for_each(|desc| guard.get_pin_mut().free(desc));
        guard.wakeup();
    }

    pub fn intr(mut self: Pin<&mut Self>) {
        // The device won't raise another interrupt until we tell it
        // we've seen this interrupt, which the following line does.
        // This may race with the device writing new entries to
        // the "used" ring, in which case we may process the new
        // completion entries in this interrupt, and have nothing to do
        // in the next interrupt, which is harmless.
        MmioRegs::intr_ack_all();

        fence(Ordering::SeqCst);

        // The device increments disk.used->idx when it
        // adds an entry to the used ring.

        while self.info.used_idx != self.used.id {
            fence(Ordering::SeqCst);
            let id = self.used.ring[(self.info.used_idx as usize) % NUM].id as usize;

            assert!(!self.info.inflight[id].status, "Disk::intr status");

            // It is safe because, from the invariant, b refers to a valid
            // buffer unless it is null.
            let buf = unsafe { self.info.inflight[id].b.as_mut() }.expect("Disk::intr");

            // disk is done with buf
            buf.deref_inner_mut().disk = false;
            buf.vdisk_request_waitchannel.wakeup();

            *self.as_mut().project().info.project().used_idx += 1;
        }
    }

    /// Find a free descriptor, mark it non-free, return its index.
    fn alloc(self: Pin<&mut Self>) -> Option<Descriptor> {
        for (idx, free) in self.project().info.project().free.iter_mut().enumerate() {
            if *free {
                *free = false;
                return Some(Descriptor::new(idx));
            }
        }

        None
    }

    /// Allocate three descriptors (they need not be contiguous).
    /// Disk transfers always use three descriptors.
    fn alloc_three_descriptors(mut self: Pin<&mut Self>) -> Option<[Descriptor; 3]> {
        let mut descs = ArrayVec::<[_; 3]>::new();

        for _ in 0..3 {
            if let Some(desc) = self.as_mut().alloc() {
                descs.push(desc);
            } else {
                for desc in descs {
                    self.as_mut().free(desc);
                }
                return None;
            }
        }

        descs.into_inner().ok()
    }

    fn free(self: Pin<&mut Self>, desc: Descriptor) {
        let mut this = self.project();
        let idx = desc.idx;
        assert!(!this.info.free[idx], "Disk::free");
        this.desc[idx].addr = 0;
        this.desc[idx].len = 0;
        this.desc[idx].flags = VirtqDescFlags::FREED;
        this.desc[idx].next = 0;
        this.info.project().free[idx] = true;
        mem::forget(desc);
    }
}
