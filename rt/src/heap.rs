use crate::sys;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use freelist::Heap;

const PAGE: usize = 4096;
const MIN_GROW_PAGES: usize = 16;

struct JobHeap(UnsafeCell<Heap>);

// a job is a single thread, nothing else ever touches its heap
unsafe impl Sync for JobHeap {}

unsafe impl GlobalAlloc for JobHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let heap = &mut *self.0.get();
        let p = heap.alloc(layout);
        if !p.is_null() {
            return p;
        }
        let pages = (layout.size() + 64).div_ceil(PAGE).max(MIN_GROW_PAGES);
        match sys::grow(pages as u64) {
            Some(base) => {
                heap.add_region(base, pages * PAGE);
                heap.alloc(layout)
            }
            None => core::ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        (*self.0.get()).dealloc(ptr)
    }
}

#[global_allocator]
static HEAP: JobHeap = JobHeap(UnsafeCell::new(Heap::empty()));
