use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::error::{Error, ErrorCode, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseOwner {
    pub operation_id: String,
    pub operation: String,
}

#[derive(Debug, Default)]
pub struct BoardLease {
    owner: Mutex<Option<LeaseOwner>>,
    changed: Condvar,
}

pub struct LeaseGuard<'a> {
    lease: &'a BoardLease,
    owner: LeaseOwner,
}

impl BoardLease {
    pub fn owner(&self) -> Option<LeaseOwner> {
        self.owner.lock().expect("lease lock poisoned").clone()
    }

    pub fn acquire(&self, owner: LeaseOwner, timeout: Duration) -> Result<LeaseGuard<'_>> {
        let deadline = Instant::now() + timeout;
        let mut current = self.owner.lock().expect("lease lock poisoned");
        while current.is_some() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let held_by = current
                    .as_ref()
                    .map(|value| value.operation_id.as_str())
                    .unwrap_or("unknown");
                return Err(Error::new(
                    ErrorCode::BoardBusy,
                    "lease",
                    format!("board lease is held by operation {held_by}"),
                ));
            }
            let (next, _) = self
                .changed
                .wait_timeout(current, remaining)
                .expect("lease lock poisoned");
            current = next;
        }
        *current = Some(owner.clone());
        Ok(LeaseGuard { lease: self, owner })
    }
}

impl Drop for LeaseGuard<'_> {
    fn drop(&mut self) {
        let mut current = self.lease.owner.lock().expect("lease lock poisoned");
        if current.as_ref() == Some(&self.owner) {
            *current = None;
            self.lease.changed.notify_one();
        }
    }
}
