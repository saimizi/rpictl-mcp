use std::collections::VecDeque;
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct SerialChunk {
    pub cursor: u64,
    pub generation: u64,
    pub timestamp: SystemTime,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct SerialSnapshot {
    pub chunks: Vec<SerialChunk>,
    pub next_cursor: u64,
    pub truncated: bool,
}

#[derive(Debug)]
pub struct SerialRing {
    capacity: usize,
    bytes: usize,
    next_cursor: u64,
    generation: u64,
    chunks: VecDeque<SerialChunk>,
}

impl SerialRing {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            capacity,
            bytes: 0,
            next_cursor: 0,
            generation: 0,
            chunks: VecDeque::new(),
        }
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn next_cursor(&self) -> u64 {
        self.next_cursor
    }
    pub fn mark_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }
    pub fn append(&mut self, timestamp: SystemTime, input: &[u8]) -> u64 {
        let original_cursor = self.next_cursor;
        self.next_cursor += input.len() as u64;
        if input.is_empty() {
            return original_cursor;
        }
        let kept = if input.len() > self.capacity {
            &input[input.len() - self.capacity..]
        } else {
            input
        };
        let cursor = self.next_cursor - kept.len() as u64;
        self.chunks.push_back(SerialChunk {
            cursor,
            generation: self.generation,
            timestamp,
            bytes: kept.to_vec(),
        });
        self.bytes += kept.len();
        while self.bytes > self.capacity {
            let excess = self.bytes - self.capacity;
            let front = self.chunks.front_mut().expect("ring is nonempty");
            if excess < front.bytes.len() {
                front.bytes.drain(..excess);
                front.cursor += excess as u64;
                self.bytes -= excess;
            } else {
                self.bytes -= front.bytes.len();
                self.chunks.pop_front();
            }
        }
        original_cursor
    }
    pub fn snapshot_from(&self, cursor: u64, max_bytes: usize) -> SerialSnapshot {
        let oldest = self
            .chunks
            .front()
            .map(|chunk| chunk.cursor)
            .unwrap_or(self.next_cursor);
        let effective_start = cursor.max(oldest);
        let mut remaining = max_bytes;
        let mut chunks = Vec::new();
        for chunk in &self.chunks {
            let start = effective_start
                .saturating_sub(chunk.cursor)
                .min(chunk.bytes.len() as u64) as usize;
            if start == chunk.bytes.len() || remaining == 0 {
                continue;
            }
            let len = (chunk.bytes.len() - start).min(remaining);
            chunks.push(SerialChunk {
                cursor: chunk.cursor + start as u64,
                generation: chunk.generation,
                timestamp: chunk.timestamp,
                bytes: chunk.bytes[start..start + len].to_vec(),
            });
            remaining -= len;
        }
        let returned = chunks
            .iter()
            .map(|chunk| chunk.bytes.len() as u64)
            .sum::<u64>();
        SerialSnapshot {
            chunks,
            next_cursor: effective_start + returned,
            truncated: cursor < oldest || effective_start + returned < self.next_cursor,
        }
    }

    pub fn snapshot_generation(&self, generation: u64, max_bytes: usize) -> SerialSnapshot {
        let available = self
            .chunks
            .iter()
            .filter(|chunk| chunk.generation == generation)
            .map(|chunk| chunk.bytes.len())
            .sum::<usize>();
        let skip = available.saturating_sub(max_bytes);
        let mut seen = 0;
        let mut chunks = Vec::new();
        for chunk in self
            .chunks
            .iter()
            .filter(|chunk| chunk.generation == generation)
        {
            let start = skip.saturating_sub(seen).min(chunk.bytes.len());
            seen += chunk.bytes.len();
            if start < chunk.bytes.len() {
                chunks.push(SerialChunk {
                    cursor: chunk.cursor + start as u64,
                    generation,
                    timestamp: chunk.timestamp,
                    bytes: chunk.bytes[start..].to_vec(),
                });
            }
        }
        let start_cursor = chunks
            .first()
            .map(|chunk| chunk.cursor)
            .unwrap_or(self.next_cursor);
        let returned = chunks
            .iter()
            .map(|chunk| chunk.bytes.len() as u64)
            .sum::<u64>();
        SerialSnapshot {
            chunks,
            next_cursor: start_cursor + returned,
            truncated: available > max_bytes,
        }
    }
}
