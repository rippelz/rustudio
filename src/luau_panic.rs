//! Luau Panic Mode: with more than one open script failing to parse, the
//! dock layout is shuffled, the window is drawn mirrored
//! (vendor/egui/src/mirror.rs) and the editor's text switches to Mandarin, through
//! egui's text hook (vendor/egui/src/text_filter.rs: every label, button and
//! painted text, not code or logs). Fixing the scripts back down to one error
//! puts the layout and English back.
//! (Not a Studio behaviour: requested in the project Discord.)

use std::sync::atomic::{AtomicBool, Ordering};

use egui_dock::{DockState, NodeIndex};

use crate::app::Tab;

/// More syntax errors than this across open scripts starts the panic.
const ERROR_LIMIT: usize = 1;

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether the panic is on.
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
        egui::text_filter::set(translate);
        match (errors > ERROR_LIMIT, self.saved.is_some()) {
            (true, false) => {
                self.saved = Some(dock.clone());
                *dock = shuffled(dock);
                ACTIVE.store(true, Ordering::Relaxed);
                egui::mirror::set(true);
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
                egui::mirror::set(false);
            }
            _ => {}
        }
    }
}

/// The pointer's position mirrored like the window, so a click lands on
/// what is drawn under it.
pub fn mirror_input(ctx: &egui::Context, raw: &mut egui::RawInput) {
    if !egui::mirror::is_on() {
        return;
    }
    let screen = ctx.viewport_rect();
    for event in &mut raw.events {
        let pos = match event {
            egui::Event::PointerMoved(pos) | egui::Event::PointerButton { pos, .. } | egui::Event::Touch { pos, .. } => pos,
            _ => continue,
        };
        pos.x = egui::mirror::x(screen, pos.x);
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

/// egui's text filter: a known label as a whole, else word by word
/// (CamelCase split), keeping words without a translation.
fn translate(text: &str) -> Option<String> {
    if !active() || !text.bytes().any(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let trimmed = text.trim();
    if let Some(whole) = zh(trimmed) {
        return Some(text.replace(trimmed, whole));
    }
    let mut out = String::with_capacity(text.len() * 2);
    let mut changed = false;
    let mut rest = text;
    while let Some(start) = rest.find(|c: char| c.is_ascii_alphabetic()) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
        // (letters touching digits are ids and hex, not words)
        let in_id = out.ends_with(|c: char| c.is_ascii_digit()) || rest[end..].starts_with(|c: char| c.is_ascii_digit());
        match (!in_id).then(|| word(&rest[..end])).flatten() {
            Some(t) => {
                out.push_str(&t);
                changed = true;
            }
            None => out.push_str(&rest[..end]),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    changed.then(|| without_spaces_between_cjk(&out))
}

/// One word, or its CamelCase parts, in Mandarin.
fn word(w: &str) -> Option<String> {
    if let Some(t) = zh(w).or_else(|| zh_word(&w.to_ascii_lowercase())) {
        return Some(t.to_string());
    }
    let starts: Vec<usize> = w.char_indices().filter(|(i, c)| *i == 0 || c.is_ascii_uppercase()).map(|(i, _)| i).collect();
    if starts.len() < 2 {
        return None;
    }
    let mut out = String::new();
    let mut changed = false;
    for (n, &a) in starts.iter().enumerate() {
        let part = &w[a..starts.get(n + 1).copied().unwrap_or(w.len())];
        match zh(part).or_else(|| zh_word(&part.to_ascii_lowercase())) {
            Some(t) => {
                out.push_str(t);
                changed = true;
            }
            None => out.push_str(part),
        }
    }
    changed.then_some(out)
}

/// Chinese doesn't put spaces between words.
fn without_spaces_between_cjk(s: &str) -> String {
    let cjk = |c: Option<char>| c.is_some_and(|c| c >= '\u{2E80}');
    let chars: Vec<char> = s.chars().collect();
    chars
        .iter()
        .enumerate()
        .filter(|&(i, &c)| !(c == ' ' && i > 0 && cjk(Some(chars[i - 1])) && cjk(chars.get(i + 1).copied())))
        .map(|(_, &c)| c)
        .collect()
}

/// A whole label (as written) in Mandarin.
fn zh(label: &str) -> Option<&'static str> {
    Some(match label {
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
        _ => return None,
    })
}

/// A lowercase word in Mandarin.
fn zh_word(w: &str) -> Option<&'static str> {
    Some(match w {
        "a" => "一个",
        "about" => "关于",
        "above" => "上方",
        "accessory" => "配饰",
        "account" => "账户",
        "action" => "操作",
        "actions" => "操作",
        "active" => "活动",
        "activity" => "活动",
        "add" => "添加",
        "added" => "已添加",
        "advanced" => "高级",
        "align" => "对齐",
        "all" => "全部",
        "allow" => "允许",
        "alpha" => "透明",
        "amount" => "数量",
        "an" => "一个",
        "analysis" => "分析",
        "and" => "和",
        "angle" => "角度",
        "angular" => "角",
        "animation" => "动画",
        "any" => "任何",
        "api" => "接口",
        "apply" => "应用",
        "are" => "是",
        "area" => "区域",
        "as" => "作为",
        "asset" => "资源",
        "assets" => "资源",
        "at" => "在",
        "attach" => "附加",
        "attachment" => "附件",
        "attribute" => "特性",
        "attributes" => "特性",
        "audio" => "音频",
        "auto" => "自动",
        "automatic" => "自动",
        "available" => "可用",
        "avatar" => "头像",
        "axis" => "轴",
        "back" => "后",
        "background" => "背景",
        "bar" => "栏",
        "base" => "基础",
        "baseplate" => "底板",
        "be" => "是",
        "been" => "已经",
        "before" => "之前",
        "behavior" => "行为",
        "below" => "下方",
        "bevel" => "斜面",
        "black" => "黑色",
        "blue" => "蓝色",
        "body" => "身体",
        "bold" => "粗体",
        "border" => "边框",
        "both" => "两者",
        "bottom" => "底部",
        "box" => "框",
        "brick" => "砖块",
        "bright" => "亮",
        "brightness" => "亮度",
        "browse" => "浏览",
        "browser" => "浏览器",
        "brush" => "笔刷",
        "build" => "构建",
        "button" => "按钮",
        "by" => "按",
        "call" => "调用",
        "camera" => "摄像机",
        "can" => "可以",
        "cancel" => "取消",
        "cast" => "投射",
        "category" => "类别",
        "center" => "中心",
        "change" => "更改",
        "changes" => "更改",
        "character" => "角色",
        "chat" => "聊天",
        "check" => "检查",
        "checking" => "正在检查",
        "child" => "子级",
        "children" => "子级",
        "class" => "类",
        "classes" => "类",
        "clear" => "清除",
        "click" => "点击",
        "client" => "客户端",
        "clip" => "剪辑",
        "clock" => "时钟",
        "close" => "关闭",
        "cloud" => "云",
        "code" => "代码",
        "collaborate" => "协作",
        "collaborators" => "协作者",
        "collide" => "碰撞",
        "collision" => "碰撞",
        "color" => "颜色",
        "colors" => "颜色",
        "command" => "命令",
        "config" => "配置",
        "connect" => "连接",
        "connected" => "已连接",
        "constraint" => "约束",
        "constraints" => "约束",
        "content" => "内容",
        "context" => "上下文",
        "contexts" => "上下文",
        "cookie" => "饼干",
        "copy" => "复制",
        "corner" => "角",
        "create" => "创建",
        "creator" => "创作者",
        "current" => "当前",
        "custom" => "自定义",
        "cut" => "剪切",
        "dark" => "深色",
        "data" => "数据",
        "database" => "数据库",
        "default" => "默认",
        "delete" => "删除",
        "density" => "密度",
        "depth" => "深度",
        "description" => "描述",
        "detail" => "细节",
        "diagnostics" => "诊断",
        "disable" => "禁用",
        "disabled" => "已禁用",
        "display" => "显示",
        "distance" => "距离",
        "do" => "做",
        "document" => "文档",
        "done" => "完成",
        "down" => "下",
        "draft" => "草稿",
        "drafts" => "草稿",
        "drag" => "拖动",
        "draw" => "绘制",
        "duplicate" => "复制",
        "duration" => "持续时间",
        "edit" => "编辑",
        "editor" => "编辑器",
        "effect" => "效果",
        "effects" => "效果",
        "elasticity" => "弹性",
        "emit" => "发射",
        "empty" => "空",
        "enable" => "启用",
        "enabled" => "已启用",
        "end" => "结束",
        "enter" => "输入",
        "error" => "错误",
        "errors" => "错误",
        "event" => "事件",
        "events" => "事件",
        "exit" => "退出",
        "explorer" => "资源管理器",
        "export" => "导出",
        "face" => "面",
        "faces" => "面",
        "false" => "否",
        "file" => "文件",
        "files" => "文件",
        "fill" => "填充",
        "filter" => "筛选",
        "find" => "查找",
        "first" => "第一",
        "fit" => "适应",
        "flat" => "平",
        "fluid" => "流体",
        "focus" => "焦点",
        "fog" => "雾",
        "folder" => "文件夹",
        "font" => "字体",
        "for" => "为",
        "force" => "力",
        "forces" => "力",
        "format" => "格式",
        "forward" => "向前",
        "frame" => "框架",
        "friction" => "摩擦",
        "from" => "从",
        "front" => "前",
        "full" => "完整",
        "game" => "游戏",
        "general" => "常规",
        "generate" => "生成",
        "generator" => "生成器",
        "geometric" => "几何",
        "get" => "获取",
        "glass" => "玻璃",
        "global" => "全局",
        "go" => "前往",
        "gravity" => "重力",
        "green" => "绿色",
        "grid" => "网格",
        "group" => "组",
        "groups" => "组",
        "gui" => "界面",
        "has" => "有",
        "have" => "有",
        "health" => "生命值",
        "height" => "高度",
        "help" => "帮助",
        "hidden" => "隐藏",
        "hide" => "隐藏",
        "history" => "历史",
        "home" => "主页",
        "horizontal" => "水平",
        "hover" => "悬停",
        "icon" => "图标",
        "id" => "编号",
        "image" => "图像",
        "import" => "导入",
        "in" => "在",
        "increment" => "增量",
        "info" => "信息",
        "input" => "输入",
        "insert" => "插入",
        "inspect" => "检查",
        "instance" => "实例",
        "instances" => "实例",
        "into" => "到",
        "inventory" => "物品",
        "invite" => "邀请",
        "is" => "是",
        "it" => "它",
        "item" => "项目",
        "items" => "项目",
        "join" => "加入",
        "joint" => "关节",
        "jump" => "跳跃",
        "key" => "键",
        "keyboard" => "键盘",
        "label" => "标签",
        "language" => "语言",
        "last" => "最后",
        "layer" => "图层",
        "layout" => "布局",
        "left" => "左",
        "length" => "长度",
        "level" => "级别",
        "light" => "光",
        "lighting" => "光照",
        "line" => "行",
        "linear" => "线性",
        "lines" => "行",
        "link" => "链接",
        "list" => "列表",
        "live" => "实时",
        "load" => "加载",
        "loaded" => "已加载",
        "local" => "本地",
        "localization" => "本地化",
        "lock" => "锁定",
        "locked" => "已锁定",
        "log" => "日志",
        "look" => "外观",
        "loop" => "循环",
        "main" => "主",
        "manage" => "管理",
        "manager" => "管理器",
        "mass" => "质量",
        "material" => "材质",
        "materials" => "材质",
        "max" => "最大",
        "medium" => "中等",
        "menu" => "菜单",
        "mesh" => "网格",
        "message" => "消息",
        "messages" => "消息",
        "min" => "最小",
        "mode" => "模式",
        "model" => "模型",
        "module" => "模块",
        "more" => "更多",
        "move" => "移动",
        "my" => "我的",
        "name" => "名称",
        "network" => "网络",
        "new" => "新建",
        "next" => "下一个",
        "no" => "没有",
        "none" => "无",
        "normal" => "正常",
        "not" => "不",
        "nothing" => "无",
        "number" => "数字",
        "object" => "对象",
        "objects" => "对象",
        "of" => "的",
        "off" => "关",
        "offset" => "偏移",
        "on" => "开",
        "only" => "仅",
        "open" => "打开",
        "options" => "选项",
        "or" => "或",
        "order" => "顺序",
        "orientation" => "方向",
        "origin" => "原点",
        "other" => "其他",
        "out" => "出",
        "output" => "输出",
        "over" => "上",
        "page" => "页面",
        "pane" => "窗格",
        "panel" => "面板",
        "parent" => "父级",
        "part" => "零件",
        "parts" => "零件",
        "paste" => "粘贴",
        "path" => "路径",
        "pause" => "暂停",
        "performance" => "性能",
        "permissions" => "权限",
        "physical" => "物理",
        "physics" => "物理",
        "pivot" => "枢轴",
        "place" => "场景",
        "plastic" => "塑料",
        "play" => "开始",
        "player" => "玩家",
        "players" => "玩家",
        "please" => "请",
        "plugin" => "插件",
        "plugins" => "插件",
        "point" => "点",
        "position" => "位置",
        "preview" => "预览",
        "primary" => "主要",
        "priority" => "优先级",
        "profiler" => "分析器",
        "project" => "项目",
        "properties" => "属性",
        "property" => "属性",
        "publish" => "发布",
        "put" => "放",
        "quality" => "质量",
        "query" => "查询",
        "quick" => "快速",
        "range" => "范围",
        "ready" => "就绪",
        "recent" => "最近",
        "red" => "红色",
        "redo" => "重做",
        "reflectance" => "反射率",
        "reflection" => "反射",
        "refresh" => "刷新",
        "remote" => "远程",
        "remove" => "移除",
        "rename" => "重命名",
        "render" => "渲染",
        "replicated" => "复制",
        "reset" => "重置",
        "resize" => "调整大小",
        "resizeable" => "可调整",
        "restore" => "还原",
        "resume" => "继续",
        "right" => "右",
        "root" => "根",
        "rotate" => "旋转",
        "rotation" => "旋转",
        "run" => "运行",
        "running" => "运行中",
        "save" => "保存",
        "scale" => "缩放",
        "scene" => "场景",
        "screen" => "屏幕",
        "script" => "脚本",
        "scripts" => "脚本",
        "search" => "搜索",
        "security" => "安全",
        "select" => "选择",
        "selected" => "已选",
        "selection" => "选择",
        "server" => "服务器",
        "service" => "服务",
        "session" => "会话",
        "set" => "设置",
        "settings" => "设置",
        "setup" => "设置",
        "shadow" => "阴影",
        "shadows" => "阴影",
        "shape" => "形状",
        "show" => "显示",
        "side" => "侧",
        "sign" => "登录",
        "signed" => "已登录",
        "size" => "大小",
        "sky" => "天空",
        "smooth" => "平滑",
        "snap" => "吸附",
        "sound" => "声音",
        "sounds" => "声音",
        "source" => "源",
        "spawn" => "重生",
        "speed" => "速度",
        "start" => "开始",
        "starter" => "初始",
        "stats" => "统计",
        "status" => "状态",
        "step" => "步",
        "stop" => "停止",
        "storage" => "存储",
        "store" => "商店",
        "studio" => "工作室",
        "studs" => "螺柱",
        "style" => "样式",
        "summary" => "摘要",
        "surface" => "表面",
        "switch" => "切换",
        "sync" => "同步",
        "system" => "系统",
        "tab" => "标签页",
        "table" => "表",
        "tag" => "标签",
        "tags" => "标签",
        "team" => "团队",
        "terrain" => "地形",
        "test" => "测试",
        "text" => "文本",
        "texture" => "纹理",
        "the" => "这",
        "theme" => "主题",
        "this" => "此",
        "time" => "时间",
        "to" => "到",
        "toggle" => "切换",
        "tool" => "工具",
        "toolbox" => "工具箱",
        "tools" => "工具",
        "top" => "顶部",
        "touch" => "触碰",
        "transform" => "变换",
        "transparency" => "透明度",
        "true" => "是",
        "type" => "类型",
        "ui" => "界面",
        "undo" => "撤销",
        "union" => "联合",
        "unique" => "唯一",
        "unlock" => "解锁",
        "up" => "上",
        "update" => "更新",
        "use" => "使用",
        "user" => "用户",
        "using" => "使用",
        "value" => "值",
        "variant" => "变体",
        "velocity" => "速度",
        "version" => "版本",
        "vertical" => "垂直",
        "view" => "视图",
        "viewport" => "视口",
        "visibility" => "可见性",
        "visible" => "可见",
        "voice" => "语音",
        "volume" => "音量",
        "walk" => "行走",
        "watch" => "监视",
        "weight" => "重量",
        "weld" => "焊接",
        "white" => "白色",
        "width" => "宽度",
        "window" => "窗口",
        "with" => "与",
        "workspace" => "工作区",
        "world" => "世界",
        "yellow" => "黄色",
        "yes" => "是",
        "yet" => "尚",
        "you" => "你",
        "your" => "你的",
        "zoom" => "缩放",
        _ => return None,
    })
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
        assert!(active() && egui::mirror::is_on());
        assert_eq!(translate("Explorer").as_deref(), Some("资源管理器"));
        assert_eq!(translate("PivotOffset").as_deref(), Some("枢轴偏移"));
        assert_eq!(translate("1 selected").as_deref(), Some("1 已选"));
        assert_eq!(translate("175a9455db8"), None);
        let mut tabs: Vec<_> = dock.iter_all_tabs().map(|(_, t)| format!("{t:?}")).collect();
        tabs.sort();
        assert_eq!(tabs, ["Explorer", "Output", "Properties", "Viewport"]);

        panic.update(&mut dock, 1);
        assert!(!active() && !egui::mirror::is_on());
        assert_eq!(translate("Explorer"), None);
        let after = format!("{:?}", dock.main_surface().iter().map(|n| n.iter_tabs().copied().collect::<Vec<_>>()).collect::<Vec<_>>());
        assert_eq!(before, after);
    }
}
