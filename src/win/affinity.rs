use windows_sys::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, RelationProcessorCore,
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};

use crate::win::process::{logical_processor_count, processor_group_count, system_affinity_mask};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreKind {
    Performance,
    Efficiency,
    Unknown,
}

impl CoreKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::Performance => "P 核（性能核）",
            Self::Efficiency => "E 核（能效核）",
            Self::Unknown => "CPU 核心",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CpuCoreInfo {
    pub label: String,
    pub kind: CoreKind,
    pub efficiency_class: u8,
    pub logical: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct CpuTopology {
    pub total_logical: u32,
    pub processor_groups: u16,
    pub group0_count: u32,
    pub system_mask: u64,
    pub cores: Vec<CpuCoreInfo>,
    pub p_logical: Vec<u32>,
    pub e_logical: Vec<u32>,
    pub hybrid: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreShortcut {
    pub label: String,
    pub logical: Vec<u32>,
    pub mask: u64,
}

#[derive(Clone, Debug)]
struct RawCore {
    efficiency_class: u8,
    logical: Vec<u32>,
}

impl CpuTopology {
    pub fn detect() -> Self {
        let total_logical = logical_processor_count();
        let processor_groups = processor_group_count();
        let group0_count =
            unsafe { windows_sys::Win32::System::Threading::GetActiveProcessorCount(0) };
        let system_mask = system_affinity_mask();
        let raw_cores = read_processor_cores();
        let (cores, p_logical, e_logical, hybrid) = classify_cores(raw_cores, group0_count);
        Self {
            total_logical,
            processor_groups,
            group0_count,
            system_mask,
            cores,
            p_logical,
            e_logical,
            hybrid,
        }
    }

    pub fn supports_affinity(&self) -> bool {
        self.processor_groups <= 1
    }

    pub fn all_mask(&self) -> u64 {
        if self.system_mask != 0 {
            self.system_mask
        } else {
            let count = self.group0_count.min(64);
            if count == 64 {
                u64::MAX
            } else {
                (1u64 << count) - 1
            }
        }
    }

    pub fn validate_mask(&self, mask: u64) -> Result<(), String> {
        if mask == 0 {
            return Err("至少选择一个 CPU".to_owned());
        }
        if !self.supports_affinity() {
            return Err(format!(
                "当前系统有 {} 个处理器组（>64 逻辑核），暂只支持组 0 的亲和性设置",
                self.processor_groups
            ));
        }
        if mask & !self.all_mask() != 0 {
            return Err("CPU 掩码包含当前系统不存在的处理器".to_owned());
        }
        Ok(())
    }

    pub fn core_for_logical(&self, logical: u32) -> Option<&CpuCoreInfo> {
        self.cores
            .iter()
            .find(|core| core.logical.contains(&logical))
    }

    pub fn core_shortcut(&self, label: &str) -> Option<CoreShortcut> {
        let core = self
            .cores
            .iter()
            .find(|core| core.label.eq_ignore_ascii_case(label))?;
        let logical = core
            .logical
            .iter()
            .copied()
            .filter(|logical| *logical < 64)
            .collect::<Vec<_>>();
        if logical.is_empty() {
            return None;
        }
        let mask = logical
            .iter()
            .fold(0u64, |mask, logical| mask | (1u64 << logical));
        Some(CoreShortcut {
            label: core.label.clone(),
            logical,
            mask,
        })
    }

    /// 生成“全部 P 核”快捷项：仅在混合架构且同时存在 P/E 核时返回。
    ///
    /// 亲和性掩码只支持组 0 的 64 个逻辑 CPU，因此过滤掉编号 >= 64 的核心；
    /// 过滤后为空则返回 None，避免生成无效掩码。
    pub fn all_p_cores_shortcut(&self) -> Option<CoreShortcut> {
        if !self.hybrid || self.p_logical.is_empty() || self.e_logical.is_empty() {
            return None;
        }
        let logical = self
            .p_logical
            .iter()
            .copied()
            .filter(|logical| *logical < 64)
            .collect::<Vec<_>>();
        if logical.is_empty() {
            return None;
        }
        let mask = logical
            .iter()
            .fold(0u64, |mask, logical| mask | (1u64 << logical));
        Some(CoreShortcut {
            label: "全部 P 核".to_owned(),
            logical,
            mask,
        })
    }

    pub fn p_thread_count(&self) -> usize {
        self.p_logical.len()
    }

    pub fn e_thread_count(&self) -> usize {
        self.e_logical.len()
    }
}

fn read_processor_cores() -> Vec<RawCore> {
    let mut needed = 0u32;
    unsafe {
        GetLogicalProcessorInformationEx(RelationProcessorCore, std::ptr::null_mut(), &mut needed);
    }
    if needed == 0 {
        return Vec::new();
    }
    let mut buffer = vec![0u8; needed as usize];
    let mut returned = needed;
    let ok = unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            buffer.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
            &mut returned,
        )
    };
    if ok == 0 {
        log::warn!("GetLogicalProcessorInformationEx failed");
        return Vec::new();
    }

    let mut cores = Vec::new();
    let mut offset = 0usize;
    while offset + 8 <= returned as usize {
        let info = unsafe {
            &*(buffer.as_ptr().add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)
        };
        let entry_size = info.Size as usize;
        if entry_size < 8 || offset + entry_size > returned as usize {
            break;
        }
        if info.Relationship == RelationProcessorCore {
            let processor = unsafe { &info.Anonymous.Processor };
            let group_count = processor.GroupCount.max(1) as usize;
            let masks =
                unsafe { std::slice::from_raw_parts(processor.GroupMask.as_ptr(), group_count) };
            let mut logical = Vec::new();
            for group in masks {
                let mask = group.Mask as u64;
                for bit in 0..64u32 {
                    if mask & (1u64 << bit) != 0 {
                        logical.push((group.Group as u32) * 64 + bit);
                    }
                }
            }
            if !logical.is_empty() {
                cores.push(RawCore {
                    efficiency_class: processor.EfficiencyClass,
                    logical,
                });
            }
        }
        offset = offset.saturating_add(entry_size);
    }
    cores
}

fn classify_cores(
    raw: Vec<RawCore>,
    group0_count: u32,
) -> (Vec<CpuCoreInfo>, Vec<u32>, Vec<u32>, bool) {
    if raw.is_empty() {
        let cores = (0..group0_count)
            .map(|index| CpuCoreInfo {
                label: format!("Core {index}"),
                kind: CoreKind::Unknown,
                efficiency_class: 0,
                logical: vec![index],
            })
            .collect();
        let p_logical = (0..group0_count).collect();
        return (cores, p_logical, Vec::new(), false);
    }

    let max_class = raw
        .iter()
        .map(|core| core.efficiency_class)
        .max()
        .unwrap_or(0);
    let min_class = raw
        .iter()
        .map(|core| core.efficiency_class)
        .min()
        .unwrap_or(0);
    let hybrid = max_class != min_class;
    let mut p_counter = 0usize;
    let mut e_counter = 0usize;
    let mut c_counter = 0usize;
    let mut p_logical = Vec::new();
    let mut e_logical = Vec::new();
    let mut cores = Vec::new();

    for core in raw {
        let kind = if !hybrid {
            CoreKind::Unknown
        } else if core.efficiency_class == max_class {
            CoreKind::Performance
        } else {
            CoreKind::Efficiency
        };
        let label = match kind {
            CoreKind::Performance => {
                let label = format!("P{p_counter}");
                p_counter += 1;
                label
            }
            CoreKind::Efficiency => {
                let label = format!("E{e_counter}");
                e_counter += 1;
                label
            }
            CoreKind::Unknown => {
                let label = format!("Core {c_counter}");
                c_counter += 1;
                label
            }
        };
        match kind {
            CoreKind::Performance | CoreKind::Unknown => {
                p_logical.extend(core.logical.iter().copied())
            }
            CoreKind::Efficiency => e_logical.extend(core.logical.iter().copied()),
        }
        cores.push(CpuCoreInfo {
            label,
            kind,
            efficiency_class: core.efficiency_class,
            logical: core.logical,
        });
    }

    cores.sort_by_key(|core| {
        let rank = match core.kind {
            CoreKind::Performance => 0,
            CoreKind::Efficiency => 1,
            CoreKind::Unknown => 2,
        };
        (rank, core.logical.first().copied().unwrap_or(u32::MAX))
    });
    (cores, p_logical, e_logical, hybrid)
}

pub fn format_mask(mask: u64) -> String {
    if mask == 0 {
        return "无".to_owned();
    }
    if mask == u64::MAX {
        return "全部".to_owned();
    }
    let mut parts = Vec::new();
    let mut index = 0u32;
    while index < 64 {
        if mask & (1u64 << index) == 0 {
            index += 1;
            continue;
        }
        let start = index;
        while index + 1 < 64 && mask & (1u64 << (index + 1)) != 0 {
            index += 1;
        }
        if start == index {
            parts.push(start.to_string());
        } else {
            parts.push(format!("{start}-{index}"));
        }
        index += 1;
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_ranges() {
        assert_eq!(format_mask(0b1111), "0-3");
        assert_eq!(format_mask(0b1001), "0, 3");
        assert_eq!(format_mask(0), "无");
    }

    #[test]
    fn classifies_hybrid_cores() {
        let raw = vec![
            RawCore {
                efficiency_class: 0,
                logical: vec![16, 17, 18, 19],
            },
            RawCore {
                efficiency_class: 1,
                logical: vec![0, 1, 2, 3],
            },
        ];
        let (cores, p_logical, e_logical, hybrid) = classify_cores(raw, 20);
        assert!(hybrid);
        assert_eq!(p_logical, vec![0, 1, 2, 3]);
        assert_eq!(e_logical, vec![16, 17, 18, 19]);
        assert_eq!(cores[0].label, "P0");
        assert_eq!(cores[1].label, "E0");
    }

    fn hybrid_topology() -> CpuTopology {
        CpuTopology {
            total_logical: 4,
            processor_groups: 1,
            group0_count: 4,
            system_mask: 0b1111,
            cores: vec![
                CpuCoreInfo {
                    label: "P0".to_owned(),
                    kind: CoreKind::Performance,
                    efficiency_class: 1,
                    logical: vec![0, 1],
                },
                CpuCoreInfo {
                    label: "E0".to_owned(),
                    kind: CoreKind::Efficiency,
                    efficiency_class: 0,
                    logical: vec![2],
                },
            ],
            p_logical: vec![0, 1],
            e_logical: vec![2],
            hybrid: true,
        }
    }

    #[test]
    fn core_shortcut_masks_all_threads_of_physical_core() {
        let topology = hybrid_topology();
        let shortcut = topology.core_shortcut("P0").expect("P0");
        assert_eq!(shortcut.logical, vec![0, 1]);
        assert_eq!(shortcut.mask, 0b11);
        assert!(topology.core_shortcut("P9").is_none());
    }

    #[test]
    fn all_p_shortcut_covers_only_performance_cores() {
        let topology = CpuTopology {
            total_logical: 8,
            processor_groups: 1,
            group0_count: 8,
            system_mask: 0xFFFF,
            cores: vec![
                CpuCoreInfo {
                    label: "P0".to_owned(),
                    kind: CoreKind::Performance,
                    efficiency_class: 1,
                    logical: vec![0, 1],
                },
                CpuCoreInfo {
                    label: "P1".to_owned(),
                    kind: CoreKind::Performance,
                    efficiency_class: 1,
                    logical: vec![2, 3],
                },
                CpuCoreInfo {
                    label: "E0".to_owned(),
                    kind: CoreKind::Efficiency,
                    efficiency_class: 0,
                    logical: vec![4, 5],
                },
            ],
            p_logical: vec![0, 1, 2, 3],
            e_logical: vec![4, 5],
            hybrid: true,
        };
        let shortcut = topology.all_p_cores_shortcut().expect("全部 P 核");
        assert_eq!(shortcut.label, "全部 P 核");
        assert_eq!(shortcut.logical, vec![0, 1, 2, 3]);
        assert_eq!(shortcut.mask, 0b1111);
        assert_eq!(shortcut.mask & 0b110000, 0);
    }

    #[test]
    fn all_p_shortcut_is_none_without_hybrid_split() {
        let topology = CpuTopology {
            total_logical: 4,
            processor_groups: 1,
            group0_count: 4,
            system_mask: 0b1111,
            cores: vec![CpuCoreInfo {
                label: "Core 0".to_owned(),
                kind: CoreKind::Unknown,
                efficiency_class: 0,
                logical: vec![0, 1, 2, 3],
            }],
            p_logical: vec![0, 1, 2, 3],
            e_logical: Vec::new(),
            hybrid: false,
        };
        assert!(topology.all_p_cores_shortcut().is_none());
    }

    #[test]
    fn all_p_shortcut_skips_logical_above_group0() {
        let topology = CpuTopology {
            total_logical: 128,
            processor_groups: 2,
            group0_count: 64,
            system_mask: u64::MAX,
            cores: Vec::new(),
            p_logical: vec![64, 65],
            e_logical: vec![0],
            hybrid: true,
        };
        assert!(topology.all_p_cores_shortcut().is_none());
    }

    #[test]
    fn homogeneous_system_has_no_hybrid_split() {
        let raw = vec![RawCore {
            efficiency_class: 0,
            logical: vec![0, 1, 2, 3],
        }];
        let (cores, p_logical, e_logical, hybrid) = classify_cores(raw, 4);
        assert!(!hybrid);
        assert_eq!(p_logical, vec![0, 1, 2, 3]);
        assert!(e_logical.is_empty());
        assert_eq!(cores[0].kind, CoreKind::Unknown);
    }
}
