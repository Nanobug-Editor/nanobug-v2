//! Bounded background work; completed jobs wake the supervisor.
/// A bounded off-thread work queue. Filesystem tasks remain counted until the thread returns.
pub(in crate::plugins::session) struct Pool(async_channel::Sender<Box<dyn FnOnce() + Send>>);

impl Pool {
    pub(in crate::plugins::session) fn new(threads: usize, capacity: usize) -> Self {
        let (send, receive) = async_channel::bounded::<Box<dyn FnOnce() + Send>>(capacity);
        let supervisor = std::thread::current();
        for _ in 0..threads {
            let receive = receive.clone();
            let supervisor = supervisor.clone();
            std::thread::spawn(move || {
                while let Ok(job) = receive.recv_blocking() {
                    job();
                    supervisor.unpark();
                }
            });
        }
        Self(send)
    }

    pub(in crate::plugins::session) fn submit(&self, task: impl FnOnce() + Send + 'static) -> bool {
        self.0.try_send(Box::new(task)).is_ok()
    }
}
