use crate::frame;
use crate::sync::SpinLock;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, NonNull};

const HEAP_FRAMES: u64 = 2048;
const HDR: usize = 16;
const MIN_BLOCK: usize = 32;

struct Node {
    size: usize,
    next: Option<NonNull<Node>>,
}

pub struct Heap {
    head: Option<NonNull<Node>>,
}

unsafe impl Send for Heap {}

impl Heap {
    pub const fn empty() -> Self {
        Heap { head: None }
    }

    unsafe fn add_region(&mut self, addr: usize, size: usize) {
        let n = addr as *mut Node;
        n.write(Node {
            size,
            next: self.head,
        });
        self.head = NonNull::new(n);
    }

    unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        if layout.align() > 16 {
            return ptr::null_mut();
        }
        let need = ((layout.size().max(1)) + 15) & !15;
        let total = need + HDR;

        let mut prev: Option<NonNull<Node>> = None;
        let mut cur = self.head;
        while let Some(mut node) = cur {
            let addr = node.as_ptr() as usize;
            let size = node.as_ref().size;
            if size >= total {
                let next = node.as_ref().next;
                let taken = if size >= total + HDR + MIN_BLOCK {
                    let split = (addr + total) as *mut Node;
                    split.write(Node {
                        size: size - total,
                        next,
                    });
                    node.as_mut().size = total;
                    NonNull::new(split)
                } else {
                    next
                };
                match prev {
                    None => self.head = taken,
                    Some(mut p) => p.as_mut().next = taken,
                }
                return (addr + HDR) as *mut u8;
            }
            prev = Some(node);
            cur = node.as_ref().next;
        }
        ptr::null_mut()
    }

    unsafe fn dealloc(&mut self, ptr: *mut u8) {
        let addr = (ptr as usize) - HDR;
        let n = addr as *mut Node;
        let size = n.read().size;

        let mut prev: Option<NonNull<Node>> = None;
        let mut cur = self.head;
        while let Some(node) = cur {
            if (node.as_ptr() as usize) > addr {
                break;
            }
            prev = Some(node);
            cur = node.as_ref().next;
        }

        (*n).next = cur;
        (*n).size = size;

        // merge with next
        if let Some(nx) = cur {
            if addr + size == nx.as_ptr() as usize {
                (*n).size += nx.as_ref().size;
                (*n).next = nx.as_ref().next;
            }
        }

        match prev {
            None => self.head = NonNull::new(n),
            Some(mut p) => {
                if p.as_ptr() as usize + p.as_ref().size == addr {
                    p.as_mut().size += (*n).size;
                    p.as_mut().next = (*n).next;
                } else {
                    p.as_mut().next = NonNull::new(n);
                }
            }
        }
    }
}

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

pub fn init() {
    let base = frame::alloc_contiguous(HEAP_FRAMES).expect("heap: no contiguous region");
    let mut h = HEAP.0.lock();
    unsafe {
        h.add_region(
            crate::paging::phys_to_virt(base) as usize,
            (HEAP_FRAMES * frame::FRAME) as usize,
        );
    }
}
