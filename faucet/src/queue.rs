use crate::error::ProblemDetail;
use serde::Serialize;
use tokio::sync::oneshot;

#[derive(Debug, Serialize, Clone)]
pub struct SuccessResponse {
    pub successful: bool,
    pub hash: String,
    pub envelope_xdr: String,
}

pub struct PendingEntry {
    pub address: String,
    pub responder: oneshot::Sender<Result<SuccessResponse, ProblemDetail>>,
}

pub struct FaucetQueue {
    entries: std::sync::Mutex<Vec<PendingEntry>>,
    max_size: usize,
}

impl FaucetQueue {
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: std::sync::Mutex::new(Vec::new()),
            max_size,
        }
    }

    pub fn enqueue(
        &self,
        addr: String,
    ) -> Result<oneshot::Receiver<Result<SuccessResponse, ProblemDetail>>, ProblemDetail> {
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= self.max_size {
            return Err(ProblemDetail::queue_full());
        }
        if entries.iter().any(|e| e.address == addr) {
            return Err(ProblemDetail::already_pending(&addr));
        }
        let (tx, rx) = oneshot::channel();
        entries.push(PendingEntry {
            address: addr,
            responder: tx,
        });
        Ok(rx)
    }

    pub fn drain(&self) -> Vec<PendingEntry> {
        let mut entries = self.entries.lock().unwrap();
        std::mem::take(&mut *entries)
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enqueue_and_drain() {
        let q = FaucetQueue::new(10);
        let _rx = q.enqueue("GABC".into()).unwrap();
        assert_eq!(q.len(), 1);
        let entries = q.drain();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].address, "GABC");
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn test_duplicate_address_rejected() {
        let q = FaucetQueue::new(10);
        let _rx = q.enqueue("GABC".into()).unwrap();
        let err = q.enqueue("GABC".into()).unwrap_err();
        assert_eq!(err.status, 409);
        assert_eq!(err.title, "Bad Request");
    }

    #[test]
    fn test_queue_full() {
        let q = FaucetQueue::new(2);
        let _rx1 = q.enqueue("G1".into()).unwrap();
        let _rx2 = q.enqueue("G2".into()).unwrap();
        let err = q.enqueue("G3".into()).unwrap_err();
        assert_eq!(err.status, 503);
        assert_eq!(err.title, "Bad Request");
    }

    #[test]
    fn test_drain_empties_queue() {
        let q = FaucetQueue::new(10);
        let entries = q.drain();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_len() {
        let q = FaucetQueue::new(10);
        assert_eq!(q.len(), 0);
        let _rx1 = q.enqueue("A".into()).unwrap();
        assert_eq!(q.len(), 1);
        let _rx2 = q.enqueue("B".into()).unwrap();
        assert_eq!(q.len(), 2);
        q.drain();
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn test_concurrent_enqueue() {
        use std::sync::Arc;
        let q = Arc::new(FaucetQueue::new(100));
        let mut handles = vec![];
        for i in 0..10 {
            let q = q.clone();
            handles.push(std::thread::spawn(move || {
                q.enqueue(format!("addr_{i}")).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(q.len(), 10);
    }
}
