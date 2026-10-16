//! Shared worker resources whose last owner must never destruct on the UI thread.
use std::{ops::Deref, sync::Arc};

pub struct Deferred<T: Send + Sync + 'static> {
    value: Option<Arc<T>>,
    retire: Arc<dyn Fn(Arc<T>) + Send + Sync>,
}
impl<T: Send + Sync + 'static> Deferred<T> {
    pub fn new(value: Arc<T>, retire: impl Fn(Arc<T>) + Send + Sync + 'static) -> Self {
        Self {
            value: Some(value),
            retire: Arc::new(retire),
        }
    }
    /// Only call inside an already-running background worker, before cache deletion.
    pub fn close_on_worker(mut self) {
        drop(self.value.take());
    }
}
impl<T: Send + Sync + 'static> Clone for Deferred<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            retire: self.retire.clone(),
        }
    }
}
impl<T: Send + Sync + 'static> Deref for Deferred<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.value.as_deref().expect("live deferred resource")
    }
}
impl<T: Send + Sync + 'static> Drop for Deferred<T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            (self.retire)(value);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_pending_clear_and_window_drop_finalize_on_worker() {
        struct Probe(std::sync::mpsc::Sender<std::thread::ThreadId>);
        impl Drop for Probe {
            fn drop(&mut self) {
                self.0.send(std::thread::current().id()).unwrap();
            }
        }
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let (tx, rx) = std::sync::mpsc::channel::<Arc<Probe>>();
        let worker = std::thread::spawn(move || {
            let id = std::thread::current().id();
            for item in rx {
                drop(item);
            }
            id
        });
        let retire = move |item| tx.send(item).unwrap();
        let active = Deferred::new(Arc::new(Probe(finished_tx)), retire);
        let mut pending = Some(active.clone());
        drop(active); // Replacing an active view.
        let old = pending.replace(pending.as_ref().unwrap().clone());
        drop(old);
        drop(pending.take()); // Clearing a pending query or dropping the window.
        drop(pending);
        let destructor = finished_rx.recv().unwrap();
        assert_ne!(destructor, std::thread::current().id());
        assert_eq!(destructor, worker.join().unwrap());
    }
}
