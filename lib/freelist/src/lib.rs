#![no_std]

use core::alloc::Layout;
use core::ptr::{self, NonNull};

const HDR: usize = 16;
const MIN_BLOCK: usize = 32;

// free blocks form a list sorted by address, so dealloc can merge with both
// neighbours
#[repr(C)]
struct Node {
    size: usize,
    next: Option<NonNull<Node>>,
}

unsafe fn write_header(user: usize, size: usize, back: usize) {
    let h = (user - HDR) as *mut usize;
    h.write(size);
    h.add(1).write(back);
}

pub struct Heap {
    head: Option<NonNull<Node>>,
}

unsafe impl Send for Heap {}

impl Heap {
    pub const fn empty() -> Self {
        Heap { head: None }
    }

    // inserted like a freed block, so it merges with its neighbours
    pub unsafe fn add_region(&mut self, addr: usize, size: usize) {
        write_header(addr + HDR, size, 0);
        self.dealloc((addr + HDR) as *mut u8);
    }

    pub unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(16);
        let need = (layout.size().max(1) + 15) & !15;

        let mut prev: Option<NonNull<Node>> = None;
        let mut cur = self.head;
        while let Some(node) = cur {
            let addr = node.as_ptr() as usize;
            let size = node.as_ref().size;
            let user = (addr + HDR + align - 1) & !(align - 1);
            let total = user + need - addr;
            if size >= total {
                let next = node.as_ref().next;
                let (taken, used) = if size >= total + HDR + MIN_BLOCK {
                    let split = (addr + total) as *mut Node;
                    split.write(Node { size: size - total, next });
                    (NonNull::new(split), total)
                } else {
                    (next, size)
                };
                match prev {
                    None => self.head = taken,
                    Some(mut p) => p.as_mut().next = taken,
                }
                write_header(user, used, user - HDR - addr);
                return user as *mut u8;
            }
            prev = Some(node);
            cur = node.as_ref().next;
        }
        ptr::null_mut()
    }

    pub unsafe fn dealloc(&mut self, ptr: *mut u8) {
        let h = (ptr as usize - HDR) as *const usize;
        let size = h.read();
        let addr = ptr as usize - HDR - h.add(1).read();
        let n = addr as *mut Node;

        let mut prev: Option<NonNull<Node>> = None;
        let mut cur = self.head;
        while let Some(node) = cur {
            if (node.as_ptr() as usize) > addr {
                break;
            }
            prev = Some(node);
            cur = node.as_ref().next;
        }

        n.write(Node { size, next: cur });

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
