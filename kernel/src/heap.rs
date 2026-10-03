use crate::frame;
use crate::sync::SpinLock;
use core::alloc::{GlobalAlloc, Layout};
use freelist::Heap;

const HEAP_FRAMES: u64 = 2048;

pub struct LockedHeap(SpinLock<Heap>);

unsafe impl GlobalAlloc for LockedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0.lock().alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        self.0.lock().dealloc(ptr)
    }
}

#[global_allocator]
pub static HEAP: LockedHeap = LockedHeap(SpinLock::new(Heap::empty()));

pub fn init() -> u64 {
    let frames = HEAP_FRAMES.min(frame::total_free() / 2);
    let base = frame::alloc_contiguous(frames).expect("heap: no contiguous region");
    let mut h = HEAP.0.lock();
    unsafe {
        h.add_region(
            crate::paging::phys_to_virt(base) as usize,
            (frames * frame::FRAME) as usize,
        );
    }
    frames * frame::FRAME
}
