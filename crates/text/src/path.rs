//! 面向展示的路径缩短：把主目录前缀替换成 `~`，不泄露用户名/主目录的完整路径。
//!
//! 展示用途——输出统一用 `/` 做分隔符，不代表真实的写盘路径，不能拿回去当文件系统路径用。

use std::env;
use std::path::{Path, PathBuf};

/// 面向展示的路径缩短：若 `path` 位于 `home` 之下，用 `~` 替换主目录前缀。
///
/// 边界（移植自 oh-my-pi `render-utils.ts:685-697`）：
/// - 必须校验 home 之后紧跟分隔符，否则 `/home/foo` 会误匹配到 `/home/foobar` 这样的
///   兄弟目录；
/// - `path` 与 `home` 完全相等时返回 `"~"`；
/// - `home` 为 `None`，或 `path` 不在 `home` 之下时，原样输出（仅做分隔符归一）；
/// - Windows 路径语义大小写不敏感（NTFS 如此）：盘符与目录组件都按 ASCII 大小写不敏感
///   比较，否则 `home = C:/Users/Alice`、`path = c:/users/alice/...` 匹配不上会导致主目录
///   原样泄露在输出里，违背本函数"脱敏"的存在意义。Unix 路径本就大小写敏感，按原样比较。
#[must_use]
pub fn shorten_path(path: &Path, home: Option<&Path>) -> String {
    let normalized = normalize_separators(path);
    let Some(home) = home else {
        return normalized;
    };

    // 主目录自身末尾若带分隔符，会让 strip_prefix 剩下的 rest 不再以 '/' 开头，
    // 从而误判成"没有紧跟分隔符"——先去掉它，让比较只关心目录语义。
    let home_normalized = normalize_separators(home);
    let home_normalized = home_normalized.trim_end_matches('/');
    if home_normalized.is_empty() {
        return normalized;
    }

    if paths_equal(&normalized, home_normalized) {
        return "~".to_owned();
    }

    match strip_prefix_boundary(&normalized, home_normalized) {
        Some(rest) => format!("~{rest}"),
        None => normalized,
    }
}

/// 比较两个展示用路径字符串是否相等：Windows 上按 ASCII 大小写不敏感（NTFS/Windows 路径
/// 语义不区分大小写），其余平台按大小写敏感（真实语义如此）。
fn paths_equal(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// 若 `s` 以 `prefix` 开头（按平台语义比较大小写）、且紧跟一个 `/` 分隔符，返回分隔符
/// 之后的剩余部分（含开头的 `/`）；否则返回 `None`。避免 `/home/foo` 误匹配 `/home/foobar`。
fn strip_prefix_boundary<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let candidate = s.get(..prefix.len())?;
    let matches = if cfg!(windows) {
        candidate.eq_ignore_ascii_case(prefix)
    } else {
        candidate == prefix
    };
    if !matches {
        return None;
    }
    let rest = s.get(prefix.len()..)?;
    if rest.starts_with('/') {
        Some(rest)
    } else {
        None
    }
}

/// 从环境变量推断当前用户的主目录：Unix 读 `HOME`，Windows 读 `USERPROFILE`。
///
/// 不用 `dirs` crate——它依赖 MPL-2.0 的 `option-ext`，被 `deny.toml` 挡在外面。
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// 把路径的展示形式统一成 `/` 分隔（不改变实际路径语义，只影响输出字符串）。
fn normalize_separators(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorten_path_replaces_home_prefix_with_tilde() {
        let home = Path::new("/home/foo");
        let path = Path::new("/home/foo/documents/x.txt");
        assert_eq!(shorten_path(path, Some(home)), "~/documents/x.txt");
    }

    #[test]
    fn shorten_path_exact_home_match_returns_bare_tilde() {
        let home = Path::new("/home/foo");
        assert_eq!(shorten_path(home, Some(home)), "~");
    }

    #[test]
    fn shorten_path_does_not_false_match_sibling_directory() {
        // /home/foobar 不是 /home/foo 的子目录——不能被误判为可以缩短。
        let home = Path::new("/home/foo");
        let path = Path::new("/home/foobar/file.txt");
        assert_eq!(shorten_path(path, Some(home)), "/home/foobar/file.txt");
    }

    #[test]
    fn shorten_path_outside_home_is_unchanged() {
        let home = Path::new("/home/foo");
        let path = Path::new("/var/log/syslog");
        assert_eq!(shorten_path(path, Some(home)), "/var/log/syslog");
    }

    #[test]
    fn shorten_path_none_home_only_normalizes_separators() {
        let path = Path::new("some/relative/path");
        assert_eq!(shorten_path(path, None), "some/relative/path");
    }

    #[test]
    fn shorten_path_normalizes_backslashes_to_forward_slashes() {
        // 用字符串手工构造反斜杠路径（而不是 Path::new，后者在非 Windows 上会把
        // 反斜杠当成普通文件名字符而不是分隔符），确保测试在任何平台上语义一致。
        let home = PathBuf::from("C:/Users/foo".replace('/', std::path::MAIN_SEPARATOR_STR));
        let path = PathBuf::from(
            "C:/Users/foo/Documents/file.txt".replace('/', std::path::MAIN_SEPARATOR_STR),
        );
        assert_eq!(shorten_path(&path, Some(&home)), "~/Documents/file.txt");
    }

    #[test]
    fn shorten_path_home_with_trailing_separator_still_matches() {
        let home = Path::new("/home/foo/");
        let path = Path::new("/home/foo/bar.txt");
        assert_eq!(shorten_path(path, Some(home)), "~/bar.txt");
    }

    #[cfg(windows)]
    #[test]
    fn shorten_path_windows_drive_letter_case_insensitive() {
        // 盘符大小写不同（C: vs c:）在 Windows 上必须仍然匹配，否则主目录原样泄露。
        let home = Path::new(r"C:\Users\Alice");
        let path = Path::new(r"c:\Users\Alice\project\file.txt");
        assert_eq!(shorten_path(path, Some(home)), "~/project/file.txt");
    }

    #[cfg(windows)]
    #[test]
    fn shorten_path_windows_component_case_insensitive() {
        // 目录组件大小写不同（Alice vs alice）在 Windows 上同样要匹配。
        let home = Path::new(r"C:\Users\Alice");
        let path = Path::new(r"C:\Users\alice\Documents\notes.txt");
        assert_eq!(shorten_path(path, Some(home)), "~/Documents/notes.txt");
    }

    #[test]
    fn home_dir_matches_platform_env_var_non_destructively() {
        // 只读环境变量，不做任何 mutate——在并行测试下安全。
        let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let expected = env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty());
        assert_eq!(home_dir(), expected);
    }
}
