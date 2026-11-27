//! Luau Panic Mode: with more than one open script failing to parse, the
//! dock layout is shuffled and the editor's labels switch to Mandarin. Fixing
//! the scripts back down to one error puts the layout and English back.
//! (Not a Studio behaviour: requested in the project Discord.)

use std::sync::atomic::{AtomicBool, Ordering};

use egui_dock::{DockState, NodeIndex};

use crate::app::Tab;

/// More syntax errors than this across open scripts starts the panic.
const ERROR_LIMIT: usize = 1;

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether the panic is on (labels read through [`zh`]).
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// The layout from before the panic, put back once it ends.
#[derive(Default)]
pub struct LuauPanic {
    saved: Option<DockState<Tab>>,
}

impl LuauPanic {
    /// Starts or ends the panic from this frame's error count.
    pub fn update(&mut self, dock: &mut DockState<Tab>, errors: usize) {
        match (errors > ERROR_LIMIT, self.saved.is_some()) {
            (true, false) => {
                self.saved = Some(dock.clone());
                *dock = shuffled(dock);
                ACTIVE.store(true, Ordering::Relaxed);
            }
            (false, true) => {
                let mut restored = self.saved.take().unwrap();
                // Tabs opened or closed during the panic stay that way.
                let live: Vec<Tab> = dock.iter_all_tabs().map(|(_, t)| *t).collect();
                restored.retain_tabs(|t| live.contains(t));
                for tab in live {
                    if restored.find_tab(&tab).is_none() {
                        restored.push_to_focused_leaf(tab);
                    }
                }
                *dock = restored;
                ACTIVE.store(false, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

/// The main surface's tabs in random order, each split off a random pane in
/// a random direction. Floating windows keep their tabs.
fn shuffled(dock: &DockState<Tab>) -> DockState<Tab> {
    let mut rng = Rng::seeded();
    let mut tabs: Vec<Tab> = dock.main_surface().iter().flat_map(|n| n.iter_tabs().copied()).collect();
    if tabs.is_empty() {
        return dock.clone();
    }
    for i in (1..tabs.len()).rev() {
        tabs.swap(i, rng.below(i + 1));
    }
    let mut out = DockState::new(vec![tabs.remove(0)]);
    let mut leaves = vec![NodeIndex::root()];
    for tab in tabs {
        let parent = leaves[rng.below(leaves.len())];
        let fraction = 0.3 + 0.4 * rng.below(100) as f32 / 100.0;
        let tree = out.main_surface_mut();
        match rng.below(4) {
            0 => tree.split_left(parent, fraction, vec![tab]),
            1 => tree.split_right(parent, fraction, vec![tab]),
            2 => tree.split_above(parent, fraction, vec![tab]),
            _ => tree.split_below(parent, fraction, vec![tab]),
        };
        // A split moves the old pane down a level: find the leaves again.
        leaves = out.main_surface().iter().enumerate().filter(|(_, n)| n.is_leaf()).map(|(i, _)| NodeIndex(i)).collect();
    }
    out
}

/// xorshift64: enough randomness for a layout, without a dependency.
struct Rng(u64);

impl Rng {
    fn seeded() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Rng(nanos | 1)
    }

    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// `label` in Mandarin while the panic is on (labels without a translation
/// stay as they are).
pub fn zh(label: &str) -> &str {
    if !active() {
        return label;
    }
    match label {
        // Dock tabs
        "Viewport" => "视口",
        "Explorer" => "资源管理器",
        "Properties" => "属性",
        "Output" => "输出",
        "Avatar Settings" => "头像设置",
        "Terrain Editor" => "地形编辑器",
        "Animation Editor" => "动画编辑器",
        "Toolbox" => "工具箱",
        "Asset Manager" => "资源管理",
        "File Sync" => "文件同步",
        "MicroProfiler" => "微型分析器",
        "Script" => "脚本",
        "Plugin" => "插件",
        // Ribbon tabs
        "File" => "文件",
        "Home" => "主页",
        "Model" => "模型",
        "Avatar" => "头像",
        "UI" => "界面",
        "Plugins" => "插件",
        "Test" => "测试",
        "View" => "视图",
        "Window" => "窗口",
        "Edit" => "编辑",
        "Help" => "帮助",
        // Ribbon buttons and menu items
        "Select" => "选择",
        "Move" => "移动",
        "Scale" => "缩放",
        "Rotate" => "旋转",
        "Transform" => "变换",
        "Part" => "零件",
        "Group" => "组合",
        "Ungroup" => "取消组合",
        "Anchor" => "锚定",
        "Lock" => "锁定",
        "Play" => "开始游戏",
        "Run" => "运行",
        "Stop" => "停止",
        "Pause" => "暂停",
        "Resume" => "继续",
        "Color" => "颜色",
        "Material" => "材质",
        "Terrain" => "地形",
        "Editor" => "编辑器",
        "Import" => "导入",
        "Generate" => "生成",
        "Insert Object" => "插入对象",
        "New" => "新建",
        "Open" => "打开",
        "Save" => "保存",
        "Save As..." => "另存为...",
        "Publish to Roblox" => "发布到 Roblox",
        "Close" => "关闭",
        "Exit" => "退出",
        "Recent" => "最近",
        "Undo" => "撤销",
        "Redo" => "重做",
        "Cut" => "剪切",
        "Copy" => "复制",
        "Paste" => "粘贴",
        "Duplicate" => "复制一份",
        "Delete" => "删除",
        "Rename" => "重命名",
        "Select All" => "全选",
        "Find in Place" => "在场景中查找",
        "Command Bar" => "命令栏",
        "Drafts" => "草稿",
        "Analysis" => "分析",
        "Run Script" => "运行脚本",
        "Activity History" => "活动历史",
        "Stats" => "统计",
        "Physics" => "物理",
        "Render" => "渲染",
        "Character" => "角色",
        "Geometric" => "几何",
        "Assets" => "资源",
        "Settings" => "设置",
        "Studio Settings" => "Studio 设置",
        _ => label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_shuffles_then_restores_the_layout() {
        let mut dock = DockState::new(vec![Tab::Viewport]);
        let tree = dock.main_surface_mut();
        let [_, right] = tree.split_right(NodeIndex::root(), 0.8, vec![Tab::Explorer, Tab::Properties]);
        tree.split_below(right, 0.7, vec![Tab::Output]);
        let before = format!("{:?}", dock.main_surface().iter().map(|n| n.iter_tabs().copied().collect::<Vec<_>>()).collect::<Vec<_>>());
        let mut panic = LuauPanic::default();

        panic.update(&mut dock, 2);
        assert!(active());
        assert_eq!(zh("Explorer"), "资源管理器");
        let mut tabs: Vec<_> = dock.iter_all_tabs().map(|(_, t)| format!("{t:?}")).collect();
        tabs.sort();
        assert_eq!(tabs, ["Explorer", "Output", "Properties", "Viewport"]);

        panic.update(&mut dock, 1);
        assert!(!active());
        assert_eq!(zh("Explorer"), "Explorer");
        let after = format!("{:?}", dock.main_surface().iter().map(|n| n.iter_tabs().copied().collect::<Vec<_>>()).collect::<Vec<_>>());
        assert_eq!(before, after);
    }
}
