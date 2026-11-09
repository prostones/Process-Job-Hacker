use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::model::policy::Policy;

pub const SVG_WIDTH: f32 = 1000.0;
const TOP_MARGIN: f32 = 16.0;
const BOTTOM_MARGIN: f32 = 16.0;
const PLOT_HEIGHT: f32 = 640.0;
pub const SVG_HEIGHT: f32 = TOP_MARGIN + PLOT_HEIGHT + BOTTOM_MARGIN;
const LEFT_X: f32 = 200.0;
const NODE_WIDTH: f32 = 13.0;
const RIGHT_X: f32 = 918.0;
const CORE_STEP: f32 = 4.0;
const RIBBON_WIDTH: f32 = 3.2;
const MIN_GAP: f32 = 2.0;
const ALL_P_MASK: u64 = 0xFFFF;

const BACKGROUND: &str = "#0f172a";
const P0_COLOR: &str = "#f59e0b";
const ALL_P_COLOR: &str = "#3b82f6";
const CUSTOM_COLOR: &str = "#22c55e";
const CPU_COLOR: &str = "#64748b";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RuleGroup {
    P0,
    AllP,
    Custom,
}

impl RuleGroup {
    fn color(self) -> &'static str {
        match self {
            Self::P0 => P0_COLOR,
            Self::AllP => ALL_P_COLOR,
            Self::Custom => CUSTOM_COLOR,
        }
    }
}

struct ProcessNode {
    name: String,
    cores: Vec<u32>,
    group: RuleGroup,
    y: f32,
    height: f32,
}

/// 渲染当前生效策略的“进程 → CPU”桑基图。
///
/// 纯图输出：只包含节点、标签和连线，不包含标题、图例与页边距。
/// 仅处理启用且带亲和性掩码的非临时策略；禁用策略即使保留掩码也不会出现。
pub fn render_svg(policies: &[Policy]) -> String {
    let nodes = collect_processes(policies);
    let height = SVG_HEIGHT;
    let mut parts = Vec::with_capacity(2048);
    parts.push(format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{SVG_WIDTH:.0}" height="{height:.0}" viewBox="0 0 {SVG_WIDTH:.0} {height:.0}">"#
    ));
    parts.push("<style>".to_owned());
    parts.push("text { font-family: 'Segoe UI', 'Microsoft YaHei', sans-serif; }".to_owned());
    parts.push(".left-label { font-size: 10px; fill: #cbd5e1; }".to_owned());
    parts.push(".right-label { font-size: 9px; fill: #94a3b8; }".to_owned());
    parts.push(".empty { font-size: 13px; fill: #94a3b8; }".to_owned());
    parts.push("</style>".to_owned());
    parts.push(format!(
        r#"<rect width="100%" height="100%" fill="{BACKGROUND}"/>"#
    ));

    if nodes.is_empty() {
        parts.push(format!(
            r#"<text class="empty" x="{:.1}" y="{:.1}" text-anchor="middle">没有可预览的亲和性策略</text>"#,
            SVG_WIDTH / 2.0,
            height / 2.0
        ));
        parts.push("</svg>".to_owned());
        return parts.join("\n");
    }

    let left_total = nodes.iter().map(|node| node.height).sum::<f32>();
    let left_gap = column_gap(nodes.len(), left_total);
    let mut cursor_y = TOP_MARGIN;
    let mut positioned = Vec::with_capacity(nodes.len());
    for mut node in nodes {
        node.y = cursor_y;
        cursor_y += node.height + left_gap;
        positioned.push(node);
    }

    let mut usage = BTreeMap::<u32, usize>::new();
    for node in &positioned {
        for core in &node.cores {
            *usage.entry(*core).or_insert(0) += 1;
        }
    }
    let right_total = usage
        .values()
        .map(|count| *count as f32 * CORE_STEP)
        .sum::<f32>();
    let right_gap = column_gap(usage.len(), right_total);

    let mut cpu_y = HashMap::with_capacity(usage.len());
    let mut cpu_cursor = HashMap::with_capacity(usage.len());
    let mut node_y = TOP_MARGIN;
    for (core, count) in &usage {
        let node_height = *count as f32 * CORE_STEP;
        cpu_y.insert(*core, node_y);
        cpu_cursor.insert(*core, node_y);
        node_y += node_height + right_gap;
    }

    let x0 = LEFT_X + NODE_WIDTH;
    let mid = (x0 + RIGHT_X) / 2.0;
    let mut ribbons = Vec::with_capacity(usage.values().sum::<usize>());
    for node in &positioned {
        let mut link_y = node.y;
        for core in &node.cores {
            let target_y = *cpu_cursor.get(core).unwrap_or(&TOP_MARGIN);
            ribbons.push(format!(
                r#"<path d="M {x0:.1} {link_y:.1} C {mid:.1} {link_y:.1} {mid:.1} {target_y:.1} {RIGHT_X:.1} {target_y:.1} L {RIGHT_X:.1} {target_bottom:.1} C {mid:.1} {target_bottom:.1} {mid:.1} {link_bottom:.1} {x0:.1} {link_bottom:.1} Z" fill="{}" fill-opacity="0.28"/>"#,
                node.group.color(),
                target_bottom = target_y + RIBBON_WIDTH,
                link_bottom = link_y + RIBBON_WIDTH,
            ));
            if let Some(value) = cpu_cursor.get_mut(core) {
                *value += CORE_STEP;
            }
            link_y += CORE_STEP;
        }
    }
    parts.extend(ribbons);

    for node in &positioned {
        parts.push(format!(
            r#"<rect x="{LEFT_X:.1}" y="{:.1}" width="{NODE_WIDTH:.1}" height="{:.1}" rx="2" fill="{}"/>"#,
            node.y,
            node.height,
            node.group.color()
        ));
        let font_size = if node.height < 12.0 { 9.0 } else { 10.0 };
        parts.push(format!(
            r#"<text class="left-label" x="{:.1}" y="{:.1}" text-anchor="end" style="font-size:{font_size:.1}px">{}</text>"#,
            LEFT_X - 8.0,
            node.y + node.height / 2.0 + 3.5,
            escape_xml(&node.name)
        ));
    }

    for (core, count) in &usage {
        let y = *cpu_y.get(core).unwrap_or(&TOP_MARGIN);
        let node_height = *count as f32 * CORE_STEP;
        parts.push(format!(
            r#"<rect x="{RIGHT_X:.1}" y="{y:.1}" width="{NODE_WIDTH:.1}" height="{node_height:.1}" rx="2" fill="{CPU_COLOR}"/>"#
        ));
        parts.push(format!(
            r#"<text class="right-label" x="{:.1}" y="{:.1}">CPU {core}</text>"#,
            RIGHT_X + NODE_WIDTH + 8.0,
            y + node_height / 2.0 + 3.0
        ));
    }

    parts.push("</svg>".to_owned());
    parts.join("\n")
}

/// 把 SVG 写入 `<exe目录>/temp/sankey-<uuid>.svg`，并清理本目录下旧的预览文件。
pub fn write_preview_svg(policies: &[Policy]) -> Result<PathBuf, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("无法定位当前程序：{error}"))?;
    let base_dir = executable
        .parent()
        .ok_or_else(|| "无法定位当前程序所在目录".to_owned())?;
    let preview_dir = base_dir.join("temp");
    std::fs::create_dir_all(&preview_dir).map_err(|error| format!("创建预览目录失败：{error}"))?;
    remove_old_previews(&preview_dir);
    let path = preview_dir.join(format!("sankey-{}.svg", Uuid::new_v4()));
    std::fs::write(&path, render_svg(policies))
        .map_err(|error| format!("写入预览文件失败：{error}"))?;
    Ok(path)
}

fn remove_old_previews(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if name.starts_with("sankey-") && name.ends_with(".svg") {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn collect_processes(policies: &[Policy]) -> Vec<ProcessNode> {
    let mut seen = HashSet::with_capacity(policies.len());
    let mut nodes = Vec::with_capacity(policies.len());
    for policy in policies {
        if policy.temporary || !policy.enabled {
            continue;
        }
        let Some(mask) = policy.limits.affinity_mask else {
            continue;
        };
        if mask == 0 {
            continue;
        }
        let name = policy.match_rule.process_name.trim();
        if name.is_empty() {
            continue;
        }
        let key = name.to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        let cores = cores_of(mask);
        if cores.is_empty() {
            continue;
        }
        nodes.push(ProcessNode {
            name: name.to_owned(),
            group: classify(mask, &cores),
            height: cores.len() as f32 * CORE_STEP,
            cores,
            y: 0.0,
        });
    }
    nodes.sort_by_key(|node| (node.group, node.name.to_lowercase()));
    nodes
}

fn cores_of(mask: u64) -> Vec<u32> {
    (0..64).filter(|bit| mask & (1u64 << bit) != 0).collect()
}

fn classify(mask: u64, cores: &[u32]) -> RuleGroup {
    if cores.len() == 2 && cores[0] == 0 && cores[1] == 1 {
        RuleGroup::P0
    } else if mask == ALL_P_MASK {
        RuleGroup::AllP
    } else {
        RuleGroup::Custom
    }
}

fn column_gap(count: usize, total_height: f32) -> f32 {
    if count <= 1 {
        0.0
    } else {
        ((PLOT_HEIGHT - total_height) / (count - 1) as f32).max(MIN_GAP)
    }
}

fn escape_xml(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::policy::{PolicyLimits, PolicyMatch};

    fn policy(name: &str, mask: u64) -> Policy {
        Policy::new(
            PolicyMatch {
                exe_path: None,
                process_name: name.to_owned(),
                cmdline_contains: None,
            },
            PolicyLimits {
                memory_bytes: None,
                affinity_mask: Some(mask),
                cpu_rate_percent: None,
                subset_affinity: false,
            },
        )
    }

    #[test]
    fn disabled_policy_is_ignored() {
        let mut disabled = policy("Disabled.exe", 0x3);
        disabled.enabled = false;
        let svg = render_svg(&[disabled]);
        assert!(!svg.contains("Disabled.exe"));
        assert!(svg.contains("没有可预览的亲和性策略"));
    }

    #[test]
    fn disabled_policy_does_not_hide_enabled_duplicate() {
        let mut disabled = policy("Chrome.exe", 0x3);
        disabled.enabled = false;
        let enabled = policy("chrome.exe", 0xFFFF);
        let svg = render_svg(&[disabled, enabled]);
        assert!(svg.contains(">chrome.exe</text>"));
        assert_eq!(svg.matches("<path ").count(), 16);
    }

    #[test]
    fn dedups_process_names_case_insensitive() {
        let policies = vec![policy("Chrome.exe", 0x3), policy("chrome.exe", 0xFFFF)];
        let svg = render_svg(&policies);
        assert_eq!(svg.matches(">Chrome.exe</text>").count(), 1);
        assert!(!svg.contains(">chrome.exe</text>"));
    }

    #[test]
    fn mask_bits_become_cpu_nodes() {
        let policies = vec![
            policy("two.exe", 0x3),
            policy("sixteen.exe", 0xFFFF),
            policy("custom.exe", 0xF0FFF),
        ];
        let svg = render_svg(&policies);
        assert!(svg.contains("CPU 0</text>"));
        assert!(svg.contains("CPU 11</text>"));
        assert!(svg.contains("CPU 15</text>"));
        assert!(svg.contains("CPU 19</text>"));
        assert!(!svg.contains("CPU 20</text>"));
        assert_eq!(svg.matches("<path ").count(), 2 + 16 + 16);
    }

    #[test]
    fn escapes_xml_special_chars() {
        let policies = vec![policy("A&B<C>.exe", 0x3)];
        let svg = render_svg(&policies);
        assert!(svg.contains("A&amp;B&lt;C&gt;.exe"));
        assert!(!svg.contains("A&B<C>.exe"));
    }

    #[test]
    fn render_svg_has_expected_canvas_and_nodes() {
        let policies = vec![policy("chrome.exe", 0xFFFF)];
        let svg = render_svg(&policies);
        assert!(svg.starts_with(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="672" viewBox="0 0 1000 672">"#
        ));
        assert!(svg.contains(">chrome.exe</text>"));
        assert!(svg.contains("CPU 0</text>"));
        assert!(svg.contains("CPU 15</text>"));
        assert!(svg.ends_with("</svg>"));
    }

    #[test]
    fn empty_policies_render_placeholder() {
        let svg = render_svg(&[]);
        assert!(svg.contains("没有可预览的亲和性策略"));
        assert!(!svg.contains("<path "));
    }
}
