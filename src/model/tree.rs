use std::collections::{HashMap, HashSet};

use crate::model::process::{PidKey, ProcessSnapshot};

#[derive(Clone, Debug)]
pub struct ProcessNode {
    pub record_index: usize,
    pub key: PidKey,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub depth: usize,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessTree {
    pub nodes: Vec<ProcessNode>,
    pub roots: Vec<usize>,
    pub index_by_key: HashMap<PidKey, usize>,
}

#[derive(Clone, Copy, Debug)]
pub struct VisibleRow {
    pub node_index: usize,
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
}

impl ProcessTree {
    #[allow(clippy::needless_range_loop)]
    pub fn build(snapshot: &ProcessSnapshot) -> Self {
        let mut nodes: Vec<ProcessNode> = snapshot
            .records
            .iter()
            .enumerate()
            .map(|(record_index, record)| ProcessNode {
                record_index,
                key: record.key,
                parent: None,
                children: Vec::new(),
                depth: 0,
            })
            .collect();

        let mut index_by_key = HashMap::with_capacity(nodes.len());
        for (index, node) in nodes.iter().enumerate() {
            index_by_key.insert(node.key, index);
        }

        let mut roots = Vec::new();
        for index in 0..nodes.len() {
            let record = &snapshot.records[nodes[index].record_index];
            let parent_index = if record.parent_pid != 0 && record.parent_pid != record.key.pid {
                snapshot
                    .by_pid
                    .get(&record.parent_pid)
                    .copied()
                    .filter(|candidate| {
                        let parent = &snapshot.records[*candidate];
                        parent.key.start_time <= record.key.start_time
                    })
            } else {
                None
            };
            nodes[index].parent = parent_index;
            if let Some(parent) = parent_index {
                nodes[parent].children.push(index);
            } else {
                roots.push(index);
            }
        }

        for index in 0..nodes.len() {
            let mut children = std::mem::take(&mut nodes[index].children);
            children.sort_by(|left, right| {
                let left = &snapshot.records[nodes[*left].record_index];
                let right = &snapshot.records[nodes[*right].record_index];
                left.name
                    .to_lowercase()
                    .cmp(&right.name.to_lowercase())
                    .then_with(|| left.key.pid.cmp(&right.key.pid))
            });
            nodes[index].children = children;
        }
        roots.sort_by(|left, right| {
            let left = &snapshot.records[nodes[*left].record_index];
            let right = &snapshot.records[nodes[*right].record_index];
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.key.pid.cmp(&right.key.pid))
        });

        // Defensive: if parent links formed a cycle, make unreachable nodes roots.
        let mut reachable = HashSet::new();
        let mut stack = roots.clone();
        while let Some(index) = stack.pop() {
            if reachable.insert(index) {
                for child in &nodes[index].children {
                    stack.push(*child);
                }
            }
        }
        for index in 0..nodes.len() {
            if !reachable.contains(&index) {
                nodes[index].parent = None;
                roots.push(index);
            }
        }

        let mut depth_stack = roots.clone();
        for root in &roots {
            nodes[*root].depth = 0;
        }
        while let Some(index) = depth_stack.pop() {
            let depth = nodes[index].depth;
            let children = nodes[index].children.clone();
            for child in children {
                nodes[child].depth = depth + 1;
                depth_stack.push(child);
            }
        }

        Self {
            nodes,
            roots,
            index_by_key,
        }
    }

    pub fn node(&self, key: PidKey) -> Option<&ProcessNode> {
        self.index_by_key
            .get(&key)
            .and_then(|index| self.nodes.get(*index))
    }

    pub fn default_expanded(&self) -> HashSet<PidKey> {
        self.nodes
            .iter()
            .filter(|node| node.depth <= 1)
            .map(|node| node.key)
            .collect()
    }

    pub fn subtree_keys(&self, root: PidKey) -> Vec<PidKey> {
        let Some(node) = self.node(root) else {
            return Vec::new();
        };
        let Some(root_index) = self.index_by_key.get(&root) else {
            return Vec::new();
        };
        let mut result = Vec::with_capacity(32);
        let mut stack = vec![*root_index];
        while let Some(index) = stack.pop() {
            result.push(self.nodes[index].key);
            for child in &self.nodes[index].children {
                stack.push(*child);
            }
        }
        let _ = node;
        result
    }

    pub fn flatten(
        &self,
        snapshot: &ProcessSnapshot,
        expanded: &HashSet<PidKey>,
        filter: &str,
        force_expand: bool,
    ) -> Vec<VisibleRow> {
        let filter = filter.trim().to_lowercase();
        let filtered = if filter.is_empty() {
            None
        } else {
            Some(self.compute_filter_visibility(snapshot, &filter))
        };

        let mut rows = Vec::new();
        let mut stack: Vec<(usize, usize)> = self
            .roots
            .iter()
            .rev()
            .map(|index| (*index, 0usize))
            .collect();
        while let Some((index, depth)) = stack.pop() {
            if let Some(visible) = &filtered
                && !visible.contains(&index)
            {
                continue;
            }
            let node = &self.nodes[index];
            let has_children = !node.children.is_empty();
            let is_expanded = force_expand || expanded.contains(&node.key) || filtered.is_some();
            rows.push(VisibleRow {
                node_index: index,
                depth,
                has_children,
                expanded: is_expanded,
            });
            if has_children && is_expanded {
                for child in node.children.iter().rev() {
                    stack.push((*child, depth + 1));
                }
            }
        }
        rows
    }

    fn compute_filter_visibility(
        &self,
        snapshot: &ProcessSnapshot,
        filter: &str,
    ) -> HashSet<usize> {
        let mut matches = HashSet::new();
        for (index, node) in self.nodes.iter().enumerate() {
            let record = &snapshot.records[node.record_index];
            let name_matches = record.name.to_lowercase().contains(filter);
            let pid_matches = record.key.pid.to_string().contains(filter);
            if name_matches || pid_matches {
                matches.insert(index);
            }
        }
        let mut visible = matches.clone();
        for index in matches {
            let mut current = self.nodes[index].parent;
            while let Some(parent) = current {
                visible.insert(parent);
                current = self.nodes[parent].parent;
            }
        }
        visible
    }
}
