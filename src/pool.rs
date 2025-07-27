use crossbeam_queue::SegQueue;

const STRING_CAPACITY: usize = 256;
static POOL: SegQueue<String> = SegQueue::new();

pub(crate) struct Pool;

impl Pool {
    #[inline(always)]
    pub(crate) fn get() -> String {
        POOL.pop()
            .unwrap_or_else(|| String::with_capacity(STRING_CAPACITY))
    }

    #[inline(always)]
    pub(crate) fn put(mut s: String) {
        s.clear();
        if s.capacity() > STRING_CAPACITY {
            s.shrink_to(STRING_CAPACITY);
        }
        POOL.push(s);
    }
}
