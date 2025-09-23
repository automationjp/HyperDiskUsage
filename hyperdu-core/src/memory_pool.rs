use std::cell::RefCell;

thread_local! {
    static TL_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

pub struct BufferGuard {
    buf: Vec<u8>,
}

impl BufferGuard {
    pub fn borrow(min_capacity: usize) -> Self {
        let mut buf = TL_BUF.with(|cell| {
            let mut v = cell.borrow_mut();
            let cap = v.capacity();
            if cap < min_capacity {
                v.reserve(min_capacity - cap);
            }
            std::mem::take(&mut *v)
        });
        if buf.len() < min_capacity {
            buf.resize(min_capacity, 0);
        }
        BufferGuard { buf }
    }

    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.buf.as_mut_slice()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

impl Drop for BufferGuard {
    fn drop(&mut self) {
        // Optionally shrink overly large buffers to cap memory usage
        let cap = self.buf.capacity();
        if cap > (4 << 20) {
            // >4MiB
            self.buf.shrink_to(1 << 20);
        }
        let mut b = Vec::new();
        std::mem::swap(&mut b, &mut self.buf);
        TL_BUF.with(|cell| {
            *cell.borrow_mut() = b;
        });
    }
}
