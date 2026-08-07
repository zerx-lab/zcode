//! 工作区根 + 路径解析。**全 crate 唯一的路径解析入口**——八个内置工具全部经由
//! [`Workspace::resolve`] 拿到磁盘路径，任何工具都不许自己 `PathBuf::join`。
//!
//! # 为什么不用 `canonicalize`
//!
//! `std::fs::canonicalize` 有三个副作用，全部与"路径解析"这个纯计算职责冲突：
//! - 要求文件已存在——`write` 工具创建新文件时目标路径必然不存在，`canonicalize` 会直接报错；
//! - 会解析符号链接，把用户写的路径悄悄换成另一个物理位置，越界判定因此对不上用户的输入；
//! - Windows 上会吐出 `\\?\` 扩展前缀，一路污染到审批提示、工具输出、TUI 展示。
//!
//! 因此本模块只做**词法**（lexical）归一：逐 component 消掉 `.` 与 `..`，不碰文件系统。
//!
//! # 为什么越界不直接报错
//!
//! [`Workspace::resolve`] 对落在工作区根之外的路径仍然返回 `Ok`（[`Resolved::outside_root`]
//! 置位），不是 `Err`。是否放行是一个**审批策略**问题——同一次越界访问，YOLO 模式下可能直接
//! 放行，严格模式下要弹审批（参考 opencode
//! `packages/opencode/src/tool/external-directory.ts:15-45` 的统一询问模型：越界不是错误，
//! 是一个需要 ask 的分支）。做成 `Err` 会强迫每个调用方都用 `match`/`?` 把这条正常业务路径
//! 当异常处理。

use std::path::{Component, Path, PathBuf};

/// 工作区根路径 + 路径解析器。
///
/// 是全 crate 唯一允许把用户/模型给出的相对路径拼接到磁盘路径的地方；工具实现只应持有
/// `Arc<Workspace>` 并调用 [`Workspace::resolve`]，不许自己 `PathBuf::join`。
#[derive(Debug, Clone)]
pub(crate) struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// 用给定的绝对根路径构造工作区。调用方负责保证 `root` 是绝对路径（通常来自
    /// [`detect_root`] 或配置里的显式覆盖）。
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// 工作区根路径。
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// 解析一个模型/用户给出的路径字符串：相对路径拼到 `root`，绝对路径原样使用，随后做
    /// 词法归一（消 `.`/`..`，不碰文件系统，见模块文档「为什么不用 `canonicalize`」）。
    ///
    /// 落在 `root` 之外时正常返回 `Ok`，`outside_root` 置位——是否放行由调用方按审批策略
    /// 决定，见模块文档「为什么越界不直接报错」。
    ///
    /// # Errors
    /// `raw` 为空串返回 [`PathError::Empty`]；归一化后的路径无法用合法 UTF-8 表示（极罕见，
    /// 通常意味着上游给了非法字节）返回 [`PathError::NotUtf8`]。
    pub(crate) fn resolve(&self, raw: &str) -> Result<Resolved, PathError> {
        if raw.is_empty() {
            return Err(PathError::Empty);
        }
        let candidate = Path::new(raw);
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.root.join(candidate)
        };
        let path = normalize_lexically(&joined);
        if path.to_str().is_none() {
            return Err(PathError::NotUtf8);
        }
        let outside_root = relative_to(&path, &self.root).is_none();
        Ok(Resolved { path, outside_root })
    }

    /// 面向展示：落在 `root` 内就给相对路径（`/` 分隔，`root` 自身显示为 `"."`）；落不进就
    /// 走 [`zcode_text::shorten_path`] 把主目录前缀换成 `~`——绝不吐出用户主目录全路径
    /// （`rule://zcode-architecture`「TUI 输出清理」硬要求）。
    #[must_use]
    pub(crate) fn display(&self, path: &Path) -> String {
        match relative_to(path, &self.root) {
            Some(rel) if rel.as_os_str().is_empty() => ".".to_owned(),
            Some(rel) => join_display(&rel),
            None => zcode_text::shorten_path(path, zcode_text::home_dir().as_deref()),
        }
    }
}

/// [`Workspace::resolve`] 的结果：归一化后的绝对路径 + 是否落在工作区根之外。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// 词法归一化之后的绝对路径（未必存在于磁盘，未必已被 `canonicalize`）。
    pub(crate) path: PathBuf,
    /// `true` 表示 `path` 不在工作区根之下，调用方需要自行决定放行还是走审批。
    pub(crate) outside_root: bool,
}

/// 路径解析失败的原因。
///
/// **没有"越界"这个变体**：越界是正常业务状态，由 [`Resolved::outside_root`] 表达，
/// 由调用方决定放行还是走审批（见模块文档）。
#[derive(Debug, thiserror::Error)]
pub(crate) enum PathError {
    /// 传入的路径字符串是空串。
    #[error("路径不能为空")]
    Empty,
    /// 归一化后的路径无法用合法 UTF-8 表示。
    #[error("路径包含无法转换为 UTF-8 的字节")]
    NotUtf8,
}

/// 从 `cwd` 向上找 git 仓库根（`.git` 目录，或 worktree 场景下指向真实 gitdir 的 `.git`
/// 文件），找不到就用 `cwd` 本身兜底——工作区根必须永远存在，不能是 `Option`。
///
/// 与 `config::paths` 的项目根探测是两件事，故意不合并成一份实现（已与 `ConfigLayer`
/// 对齐）：那边额外认 `.zcode/` 目录、且返回 `Option<PathBuf>`（"没有项目配置"对配置层是
/// 合法状态，调用方回退到默认值即可）；这边只认 `.git`，且必须兜底到 `cwd`（路径解析不能
/// 没有根，`Workspace::new` 要的是一个必然存在的 `PathBuf`)。
///
/// 判据抄自 jcode `crates/jcode-build-support/src/paths.rs:55-59,590-600`：用 `.exists()`
/// 而不是 `.is_dir()`，因为 git worktree 的 `.git` 是一个指向真实 gitdir 的文件而非目录。
pub(crate) fn detect_root(cwd: &Path) -> PathBuf {
    for ancestor in cwd.ancestors() {
        if ancestor.join(".git").exists() {
            return ancestor.to_path_buf();
        }
    }
    cwd.to_path_buf()
}

/// 逐 component 消掉 `.` 与 `..`，不访问文件系统、不解析符号链接。
///
/// `..` 越过根（`RootDir`/`Prefix`）时不 panic，直接丢弃——效果等价于 shell 里
/// `cd /` 之后再 `cd ..` 仍留在 `/`；相对路径里领先的 `..`（如 `../../foo`，理论上不会在
/// 本模块的调用路径里出现，因为相对输入总是先 join 到绝对的 `root`）会原样累积保留。
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut stack: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match stack.last() {
                Some(Component::Normal(_)) => {
                    stack.pop();
                }
                Some(Component::ParentDir) => stack.push(component),
                Some(Component::RootDir | Component::Prefix(_) | Component::CurDir) | None => {
                    // 已经到根，或还没有任何前缀部件：不能再往上，静默吞掉。
                    // `CurDir` 分支理论上不可达——上面已经把它过滤掉，从不入栈——但
                    // `match` 要求穷尽，归并进同一个"静默吞掉"分支比 `unreachable!()`
                    // 更安全（万一以后有人改动上面的过滤逻辑，这里也不会 panic）。
                }
            },
            other => stack.push(other),
        }
    }
    stack.into_iter().collect()
}

/// `path` 落在 `root` 之内（含二者相等）时返回相对部分（可能为空，表示二者相等）；否则
/// `None`。
///
/// 逐 component 比较，天然规避字符串前缀比较的边界陷阱——`root=/a`、`path=/ab` 在
/// component 层面是 `"a"` vs `"ab"`，不会被误判为前缀匹配（不需要额外写
/// `crates/text/src/path.rs:53-71` 那种手动边界校验，`Path::components()` 本身就是按
/// component 切的）。
///
/// Windows 语义大小写不敏感，Unix 按原样比较——与 `crates/text/src/path.rs:43-51` 的
/// `paths_equal` 同一套语义；那两个函数是 `zcode-text` crate 内部私有项，无法跨 crate
/// 复用，这里按相同规则重写一份（见 [`component_eq`]）。
fn relative_to(path: &Path, root: &Path) -> Option<PathBuf> {
    let mut path_components = path.components();
    for root_component in root.components() {
        match path_components.next() {
            Some(candidate) if component_eq(root_component, candidate) => {}
            _ => return None,
        }
    }
    Some(path_components.as_path().to_path_buf())
}

/// 单个路径 component 的平台相关比较：Windows 上 NTFS 路径语义大小写不敏感，按 ASCII
/// 大小写不敏感比较；其余平台按原样比较。
fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    if cfg!(windows) {
        match (a.as_os_str().to_str(), b.as_os_str().to_str()) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => a == b,
        }
    } else {
        a == b
    }
}

/// 相对路径转展示字符串：统一用 `/` 分隔，不泄露平台原始分隔符（Windows 的 `\`）。
fn join_display(rel: &Path) -> String {
    rel.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_at(root: &Path) -> Workspace {
        Workspace::new(root.to_path_buf())
    }

    #[test]
    fn relative_path_joins_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = workspace_at(tmp.path());
        let resolved = ws.resolve("src/main.rs").expect("resolve");
        assert_eq!(resolved.path, tmp.path().join("src").join("main.rs"));
        assert!(!resolved.outside_root);
    }

    #[test]
    fn absolute_path_is_used_as_is() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = workspace_at(tmp.path());
        let outside = tmp.path().parent().expect("parent").join("elsewhere");
        let raw = outside.to_str().expect("utf8 tempdir path");
        let resolved = ws.resolve(raw).expect("resolve");
        assert_eq!(resolved.path, outside);
    }

    #[test]
    fn dot_and_dotdot_components_are_eliminated() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = workspace_at(tmp.path());
        let resolved = ws.resolve("./a/../b/./c").expect("resolve");
        assert_eq!(resolved.path, tmp.path().join("b").join("c"));
        assert!(!resolved.outside_root);
    }

    #[test]
    fn dotdot_beyond_root_marks_outside_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = workspace_at(tmp.path());
        let resolved = ws.resolve("../escaped").expect("resolve");
        assert!(resolved.outside_root);
        let expected = tmp.path().parent().expect("parent").join("escaped");
        assert_eq!(resolved.path, expected);
    }

    #[test]
    fn sibling_directory_with_shared_prefix_is_not_inside_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("a");
        std::fs::create_dir(&root).expect("mkdir a");
        let sibling = tmp.path().join("ab");
        std::fs::create_dir(&sibling).expect("mkdir ab");

        let ws = workspace_at(&root);
        let raw = sibling.to_str().expect("utf8 tempdir path");
        let resolved = ws.resolve(raw).expect("resolve");
        assert!(
            resolved.outside_root,
            "root=/a must not match path=/ab by string prefix"
        );
        assert_eq!(resolved.path, sibling);
    }

    #[test]
    fn empty_string_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = workspace_at(tmp.path());
        let err = ws.resolve("").expect_err("must reject empty");
        assert!(matches!(err, PathError::Empty));
    }

    #[test]
    fn display_relative_to_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = workspace_at(tmp.path());
        let inside = tmp.path().join("src").join("lib.rs");
        assert_eq!(ws.display(&inside), "src/lib.rs");
        assert_eq!(ws.display(tmp.path()), ".");
    }

    #[test]
    fn display_outside_root_falls_back_to_shorten_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("project");
        std::fs::create_dir(&root).expect("mkdir project");
        let ws = workspace_at(&root);
        let outside = tmp.path().join("other").join("file.rs");
        let displayed = ws.display(&outside);
        // 落不进 root：不应该按 root 相对路径展示，必须是完整（或经 shorten_path 处理过的）路径。
        assert!(!displayed.starts_with("other/"));
        assert!(displayed.ends_with("file.rs"));
    }

    #[test]
    fn detect_root_finds_git_ancestor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");
        let nested = tmp.path().join("crates").join("foo");
        std::fs::create_dir_all(&nested).expect("mkdir nested");

        let root = detect_root(&nested);
        // `tempdir()` 在多数平台上不经符号链接，可以直接比较；若某平台上 tmp 根本身经过了
        // 一层符号链接（如 macOS `/tmp` -> `/private/tmp`），这里用 canonicalize 兜底比较。
        assert!(root == tmp.path() || root.canonicalize().ok() == tmp.path().canonicalize().ok());
    }

    #[test]
    fn detect_root_falls_back_to_cwd_without_git() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nested = tmp.path().join("no-git-here");
        std::fs::create_dir_all(&nested).expect("mkdir nested");
        // 上溯到系统临时目录及以上都没有 `.git`（临时目录约定不落在 git 仓库里）。
        let root = detect_root(&nested);
        assert_eq!(root, nested);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_are_case_insensitive_for_root_containment() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("Project");
        std::fs::create_dir(&root).expect("mkdir Project");
        let ws = workspace_at(&root);

        let mut upper_raw = root.to_str().expect("utf8 tempdir path").to_uppercase();
        upper_raw.push_str("\\src\\main.rs");
        let resolved = ws.resolve(&upper_raw).expect("resolve");
        assert!(
            !resolved.outside_root,
            "大小写不同的同一路径在 Windows 上应仍判定为落在 root 内"
        );
    }
}
