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
        // Property categories
        "Appearance" => "外观",
        "Data" => "数据",
        "Pivot" => "枢轴",
        "Behavior" => "行为",
        "Collision" => "碰撞",
        "Surface" => "表面",
        "Surface Inputs" => "表面输入",
        "Assembly" => "组件",
        "Attributes" => "特性",
        "Tags" => "标签",
        "Permissions" => "权限",
        "Camera" => "摄像机",
        "Text" => "文本",
        "Image" => "图像",
        "State" => "状态",
        "Shape" => "形状",
        "Controls" => "控制",
        "Teams" => "队伍",
        "Light" => "光照",
        "Environment" => "环境",
        "Fog" => "雾",
        "Exposure" => "曝光",
        "Sound" => "声音",
        "Playback" => "播放",
        "Emission" => "发射",
        "Motion" => "运动",
        "Particles" => "粒子",
        "Thrust" => "推力",
        "Goals" => "目标",
        "Layout" => "布局",
        "Rendering" => "渲染设置",
        "Localization" => "本地化",
        "Input" => "输入",
        "Scrolling" => "滚动",
        "Derive" => "派生",
        // Properties
        "BrickColor" => "砖块颜色",
        "CastShadow" => "投射阴影",
        "MaterialVariant" => "材质变体",
        "Reflectance" => "反射率",
        "Transparency" => "透明度",
        "Archivable" => "可存档",
        "ClassName" => "类名",
        "Locked" => "已锁定",
        "Name" => "名称",
        "Parent" => "父级",
        "ResizeableFaces" => "可调整面",
        "ResizeIncrement" => "调整增量",
        "UniqueId" => "唯一 ID",
        "Size" => "大小",
        "Position" => "位置",
        "Orientation" => "方向",
        "Rotation" => "旋转角度",
        "CFrame" => "坐标框架",
        "Origin" => "原点",
        "Origin Position" => "原点位置",
        "Origin Orientation" => "原点方向",
        "Pivot Offset" => "枢轴偏移",
        "Pivot Offset Position" => "枢轴偏移位置",
        "Pivot Offset Orientation" => "枢轴偏移方向",
        "WorldPivot" => "世界枢轴",
        "Anchored" => "已锚定",
        "CanCollide" => "可碰撞",
        "CanTouch" => "可触碰",
        "CanQuery" => "可查询",
        "CollisionGroup" => "碰撞组",
        "Massless" => "无质量",
        "CustomPhysicalProperties" => "自定义物理属性",
        "CurrentPhysicalProperties" => "当前物理属性",
        "RootPriority" => "根优先级",
        "AssemblyLinearVelocity" => "组件线速度",
        "AssemblyAngularVelocity" => "组件角速度",
        "EnableFluidForces" => "启用流体力",
        "Disabled" => "已禁用",
        "Enabled" => "已启用",
        "RunContext" => "运行上下文",
        "Source" => "源代码",
        "LinkedSource" => "链接源",
        "Visible" => "可见",
        "Value" => "值",
        "TextColor3" => "文本颜色",
        "TextSize" => "文本大小",
        "Font" => "字体",
        "BackgroundColor3" => "背景颜色",
        "BackgroundTransparency" => "背景透明度",
        "BorderColor3" => "边框颜色",
        "BorderSizePixel" => "边框像素大小",
        "AnchorPoint" => "锚点",
        "ZIndex" => "Z 索引",
        "LayoutOrder" => "布局顺序",
        "ImageColor3" => "图像颜色",
        "ImageTransparency" => "图像透明度",
        "Brightness" => "亮度",
        "Range" => "范围",
        "Shadows" => "阴影",
        "Ambient" => "环境光",
        "ClockTime" => "时钟时间",
        "FogColor" => "雾颜色",
        "FogEnd" => "雾结束",
        "FogStart" => "雾开始",
        "Gravity" => "重力",
        "SoundId" => "声音 ID",
        "Volume" => "音量",
        "Looped" => "循环",
        "Playing" => "正在播放",
        "PlaybackSpeed" => "播放速度",
        "MeshId" => "网格 ID",
        "TextureID" => "纹理 ID",
        "Texture" => "纹理",
        "Face" => "面",
        "Health" => "生命值",
        "MaxHealth" => "最大生命值",
        "WalkSpeed" => "行走速度",
        "JumpPower" => "跳跃力",
        "JumpHeight" => "跳跃高度",
        "PrimaryPart" => "主要零件",
        "LevelOfDetail" => "细节层次",
        "ModelStreamingMode" => "模型流式模式",
        "FieldOfView" => "视野",
        "CameraType" => "摄像机类型",
        "CameraSubject" => "摄像机主体",
        "Focus" => "焦点",
        // Services and common instances
        "Workspace" => "工作区",
        "Players" => "玩家",
        "Lighting" => "光照服务",
        "MaterialService" => "材质服务",
        "ReplicatedFirst" => "优先复制",
        "ReplicatedStorage" => "复制存储",
        "ServerScriptService" => "服务器脚本服务",
        "ServerStorage" => "服务器存储",
        "StarterGui" => "初始界面",
        "StarterPack" => "初始背包",
        "StarterPlayer" => "初始玩家",
        "StarterPlayerScripts" => "初始玩家脚本",
        "StarterCharacterScripts" => "初始角色脚本",
        "SoundService" => "声音服务",
        "Chat" => "聊天",
        "TextChatService" => "文字聊天服务",
        "LocalizationService" => "本地化服务",
        "TestService" => "测试服务",
        "VoiceChatService" => "语音聊天服务",
        "SpawnLocation" => "重生点",
        "Baseplate" => "底板",
        "LocalScript" => "本地脚本",
        "ModuleScript" => "模块脚本",
        "Folder" => "文件夹",
        "MeshPart" => "网格零件",
        "UnionOperation" => "联合体",
        "Decal" => "贴花",
        "Humanoid" => "人形",
        "Sky" => "天空",
        "Atmosphere" => "大气",
        "Clouds" => "云",
        "PointLight" => "点光源",
        "SpotLight" => "聚光灯",
        "SurfaceLight" => "表面光源",
        "ScreenGui" => "屏幕界面",
        "Frame" => "框架",
        "TextLabel" => "文本标签",
        "TextButton" => "文本按钮",
        "ImageLabel" => "图像标签",
        "ImageButton" => "图像按钮",
        "Attachment" => "附件",
        "Weld" => "焊接",
        "WeldConstraint" => "焊接约束",
        "ParticleEmitter" => "粒子发射器",
        "RemoteEvent" => "远程事件",
        "RemoteFunction" => "远程函数",
        "BindableEvent" => "可绑定事件",
        "StringValue" => "字符串值",
        "IntValue" => "整数值",
        "NumberValue" => "数值",
        "BoolValue" => "布尔值",
        "Team" => "队伍",
        "Tool" => "工具",
        "Ball" => "球",
        "Statue" => "雕像",
        "Bridge" => "桥",
        "Glass" => "玻璃",
        "Lamp" => "灯",
        "NeonStrip" => "霓虹灯条",
        "Pipe" => "管道",
        "Step" => "台阶",
        "Tower" => "塔",
        "Ramp" => "坡道",
        "PivotOffset" => "枢轴偏移",
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
