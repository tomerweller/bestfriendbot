use crate::error::ProblemDetail;
use serde::Serialize;
use tokio::sync::oneshot;

#[derive(Debug, Serialize, Clone)]
pub struct SuccessResponse {
    pub hash: String,
    pub address: String,
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
