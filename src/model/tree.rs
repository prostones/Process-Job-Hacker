use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::model::process::{PidKey, ProcessRecord, ProcessSnapshot};

#[derive(Clone, Debug)]
pub struct ProcessNode {
    pub record_index: usize,
    pub key: PidKey,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub depth: usize,
}

/// 进程树中可点击排序的列。
///
/// 规则列只是“是否已生效”的状态标识，不参与排序，因此不在此枚举中。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProcessSortKey {
    /// 进程名称，比较时忽略大小写。
    #[default]
    Name,
    /// 进程 PID。
    Pid,
    /// 父进程 PID。
    ParentPid,
    /// CPU 占用百分比。
    Cpu,
    /// 工作集内存（字节）。
    Memory,
    /// 提交内存（字节）。
    Commit,
}

impl ProcessSortKey {
    /// 首次点击该列时采用的默认方向。
    ///
    /// 资源占用类列默认“大的在前”，标识类列默认“小的在前”，符合任务管理器习惯。
    pub fn default_direction(self) -> SortDirection {
        match self {
            Self::Cpu | Self::Memory | Self::Commit => SortDirection::Descending,
            Self::Name | Self::Pid | Self::ParentPid => SortDirection::Ascending,
        }
    }
}

/// 排序方向。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortDirection {
    /// 升序（小的在前）。
    #[default]
    Ascending,
    /// 降序（大的在前）。
    Descending,
}

/// 进程树的排序规格：排序键 + 方向。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessSort {
    pub key: ProcessSortKey,
    pub direction: SortDirection,
}

impl ProcessSort {
    /// 点击表头：同列翻转方向，换列时回到该列的默认方向。
    pub fn toggle(&mut self, key: ProcessSortKey) {
        if self.key == key {
            self.direction = match self.direction {
                SortDirection::Ascending => SortDirection::Descending,
                SortDirection::Descending => SortDirection::Ascending,
            };
            return;
        }
        self.key = key;
        self.direction = key.default_direction();
    }

    /// 同一父节点下两个子节点的比较。
    ///
    /// 排序只发生在兄弟节点之间，子树跟随父节点整体移动，避免打乱父子关系。
    fn compare(self, left: &ProcessRecord, right: &ProcessRecord) -> Ordering {
        let ordering = match self.key {
            ProcessSortKey::Name => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
            ProcessSortKey::Pid => left.key.pid.cmp(&right.key.pid),
            ProcessSortKey::ParentPid => left.parent_pid.cmp(&right.parent_pid),
            ProcessSortKey::Cpu => left
                .cpu_percent
                .partial_cmp(&right.cpu_percent)
                .unwrap_or(Ordering::Equal),
            ProcessSortKey::Memory => left.memory.cmp(&right.memory),
            ProcessSortKey::Commit => left.commit.cmp(&right.commit),
        };
        let ordering = match self.direction {
            SortDirection::Ascending => ordering,
            SortDirection::Descending => ordering.reverse(),
        };
        // 同值时用 PID 兜底，降低快照刷新时行序抖动。
        ordering.then_with(|| left.key.pid.cmp(&right.key.pid))
    }
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
    /// 按默认排序（名称升序）构建进程树。
    ///
    /// 仅供测试与默认场景使用；生产代码统一走 [`Self::build_with_sort`]。
    #[cfg(test)]
    pub fn build(snapshot: &ProcessSnapshot) -> Self {
        Self::build_with_sort(snapshot, ProcessSort::default())
    }

    /// 按指定排序规格构建进程树。
    ///
    /// 排序在构建阶段完成，`flatten` 每帧只负责展开/过滤，不重复排序。
    #[allow(clippy::needless_range_loop)]
    pub fn build_with_sort(snapshot: &ProcessSnapshot, sort: ProcessSort) -> Self {
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
                sort.compare(left, right)
            });
            nodes[index].children = children;
        }
        roots.sort_by(|left, right| {
            let left = &snapshot.records[nodes[*left].record_index];
            let right = &snapshot.records[nodes[*right].record_index];
            sort.compare(left, right)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一条测试用进程记录，`start_time` 固定为 1。
    fn record(pid: u32, parent_pid: u32, name: &str, cpu: f32, memory: u64) -> ProcessRecord {
        ProcessRecord {
            key: PidKey::new(pid, 1),
            parent_pid,
            name: name.to_owned(),
            cpu_percent: cpu,
            memory,
            commit: memory,
            path: None,
        }
    }

    /// 构造 `parent -> 两个子进程` 的固定形态：PID 1 下挂 PID 2、3。
    fn sample_snapshot() -> ProcessSnapshot {
        ProcessSnapshot::new(vec![
            record(1, 0, "parent", 5.0, 100),
            record(2, 1, "zebra", 90.0, 300),
            record(3, 1, "alpha", 10.0, 200),
        ])
    }

    fn child_names(tree: &ProcessTree, snapshot: &ProcessSnapshot, parent_pid: u32) -> Vec<String> {
        let parent_index = tree.index_by_key[&PidKey::new(parent_pid, 1)];
        tree.nodes[parent_index]
            .children
            .iter()
            .map(|index| {
                snapshot.records[tree.nodes[*index].record_index]
                    .name
                    .clone()
            })
            .collect()
    }

    #[test]
    fn build_with_sort_orders_roots_by_name_ascending() {
        let snapshot = ProcessSnapshot::new(vec![
            record(10, 0, "Gamma", 0.0, 0),
            record(20, 0, "alpha", 0.0, 0),
            record(30, 0, "Beta", 0.0, 0),
        ]);
        let tree = ProcessTree::build(&snapshot);
        let names = tree
            .roots
            .iter()
            .map(|index| {
                snapshot.records[tree.nodes[*index].record_index]
                    .name
                    .clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["alpha", "Beta", "Gamma"]);
    }

    #[test]
    fn build_with_sort_orders_siblings_by_cpu_descending() {
        let snapshot = sample_snapshot();
        let sort = ProcessSort {
            key: ProcessSortKey::Cpu,
            direction: SortDirection::Descending,
        };
        let tree = ProcessTree::build_with_sort(&snapshot, sort);
        assert_eq!(child_names(&tree, &snapshot, 1), vec!["zebra", "alpha"]);
    }

    #[test]
    fn build_with_sort_keeps_parent_child_relation() {
        let snapshot = sample_snapshot();
        let sort = ProcessSort {
            key: ProcessSortKey::Cpu,
            direction: SortDirection::Descending,
        };
        let tree = ProcessTree::build_with_sort(&snapshot, sort);
        let parent_index = tree.index_by_key[&PidKey::new(1, 1)];
        for child_index in &tree.nodes[parent_index].children {
            assert_eq!(tree.nodes[*child_index].parent, Some(parent_index));
            assert_eq!(tree.nodes[*child_index].depth, 1);
        }
        assert_eq!(tree.roots, vec![parent_index]);
    }

    #[test]
    fn build_with_sort_breaks_ties_by_pid() {
        let snapshot = ProcessSnapshot::new(vec![
            record(30, 0, "same", 50.0, 0),
            record(10, 0, "same", 50.0, 0),
            record(20, 0, "same", 50.0, 0),
        ]);
        let sort = ProcessSort {
            key: ProcessSortKey::Cpu,
            direction: SortDirection::Ascending,
        };
        let tree = ProcessTree::build_with_sort(&snapshot, sort);
        let pids = tree
            .roots
            .iter()
            .map(|index| snapshot.records[tree.nodes[*index].record_index].key.pid)
            .collect::<Vec<_>>();
        assert_eq!(pids, vec![10, 20, 30]);
    }

    #[test]
    fn default_sort_is_name_ascending() {
        let sort = ProcessSort::default();
        assert_eq!(sort.key, ProcessSortKey::Name);
        assert_eq!(sort.direction, SortDirection::Ascending);
    }

    #[test]
    fn toggle_same_key_flips_direction() {
        let mut sort = ProcessSort {
            key: ProcessSortKey::Name,
            direction: SortDirection::Ascending,
        };
        sort.toggle(ProcessSortKey::Name);
        assert_eq!(sort.direction, SortDirection::Descending);
        sort.toggle(ProcessSortKey::Name);
        assert_eq!(sort.direction, SortDirection::Ascending);
    }

    #[test]
    fn toggle_new_key_uses_default_direction() {
        let mut sort = ProcessSort::default();

        sort.toggle(ProcessSortKey::Cpu);
        assert_eq!(sort.key, ProcessSortKey::Cpu);
        assert_eq!(sort.direction, SortDirection::Descending);

        sort.toggle(ProcessSortKey::Name);
        assert_eq!(sort.key, ProcessSortKey::Name);
        assert_eq!(sort.direction, SortDirection::Ascending);
    }
}
