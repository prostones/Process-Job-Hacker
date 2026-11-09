use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuLevel {
    Normal,
    Warn,
    Critical,
}

pub fn level_for_usage(usage: f32) -> CpuLevel {
    if usage > 95.0 {
        CpuLevel::Critical
    } else if usage > 80.0 {
        CpuLevel::Warn
    } else {
        CpuLevel::Normal
    }
}

#[derive(Clone, Debug)]
pub struct CpuHistory {
    pub capacity: usize,
    pub brand: String,
    pub total: VecDeque<f32>,
    pub cores: Vec<VecDeque<f32>>,
}

impl CpuHistory {
    pub fn new(capacity: usize, brand: String, core_count: usize) -> Self {
        let capacity = capacity.max(2);
        Self {
            capacity,
            brand,
            total: VecDeque::with_capacity(capacity),
            cores: (0..core_count)
                .map(|_| VecDeque::with_capacity(capacity))
                .collect(),
        }
    }

    pub fn core_count(&self) -> usize {
        self.cores.len()
    }

    pub fn push(&mut self, total: f32, cores: &[f32]) {
        if cores.len() != self.cores.len() {
            self.cores = (0..cores.len())
                .map(|_| VecDeque::with_capacity(self.capacity))
                .collect();
        }
        push_ring(&mut self.total, total, self.capacity);
        for (history, value) in self.cores.iter_mut().zip(cores.iter().copied()) {
            push_ring(history, value, self.capacity);
        }
    }

    pub fn latest_total(&self) -> f32 {
        self.total.back().copied().unwrap_or(0.0)
    }
}

fn push_ring(ring: &mut VecDeque<f32>, value: f32, capacity: usize) {
    if ring.len() == capacity {
        ring.pop_front();
    }
    ring.push_back(value.clamp(0.0, 100.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_keeps_capacity_and_latest() {
        let mut history = CpuHistory::new(3, "cpu".to_owned(), 1);
        for value in [10.0, 20.0, 30.0, 40.0] {
            history.push(value, &[value]);
        }
        assert_eq!(history.total.len(), 3);
        assert_eq!(history.total.front().copied(), Some(20.0));
        assert_eq!(history.latest_total(), 40.0);
    }

    #[test]
    fn threshold_boundaries_are_strict() {
        assert_eq!(level_for_usage(80.0), CpuLevel::Normal);
        assert_eq!(level_for_usage(80.1), CpuLevel::Warn);
        assert_eq!(level_for_usage(95.0), CpuLevel::Warn);
        assert_eq!(level_for_usage(95.1), CpuLevel::Critical);
    }

    #[test]
    fn core_count_change_resets_history() {
        let mut history = CpuHistory::new(4, "cpu".to_owned(), 2);
        history.push(10.0, &[10.0, 20.0]);
        history.push(20.0, &[20.0, 30.0, 40.0]);
        assert_eq!(history.core_count(), 3);
        assert_eq!(history.cores[0].len(), 1);
    }
}
