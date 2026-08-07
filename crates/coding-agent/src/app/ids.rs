//! [`ComponentId`] 分配：把协议 id（`EntryId`/`CallId`）与本 crate 内置的
//! 几个固定组件（状态行、待办弹窗、输入框）映射进同一个 `u64` 命名空间。
//!
//! # 为什么不能直接哈希裸字符串
//!
//! `EntryId` 与 `CallId` 都是不透明字符串，两者的取值域互不相关，但都可能出现
//! 撞在同一个哈希值上的字符串（概率极低，但账本一旦真的撞上，会把两个毫不相关
//! 的组件在 [`crate::app::transcript`] 的段缓存里错误复用，读者会看到一条消息
//! 突然"变成"另一条）。给每个命名空间前置一个专属字节再哈希，把撞车概率从
//! "任意两个协议 id 之间"收窄到"同一命名空间内部"，且给固定组件的哨兵值
//! 留出不会与任何哈希输出相撞的保留区间（`0..RESERVED_COMPONENT_IDS`）。

use zcode_tui::ComponentId;

/// 哈希产生的 `ComponentId` 一律偏移到这个值之上，把 `0..RESERVED` 整段留给
/// 下面的固定哨兵组件，物理上不可能与任何哈希输出冲突。
const RESERVED_COMPONENT_IDS: u64 = 16;

/// 状态行组件（spinner + 处理中提示）的固定 id。
pub(crate) const STATUS_COMPONENT: ComponentId = ComponentId(0);
/// 待审批 / 待 stdin 弹窗组件的固定 id。
pub(crate) const PENDING_COMPONENT: ComponentId = ComponentId(1);
/// 输入框组件的固定 id。
pub(crate) const INPUT_COMPONENT: ComponentId = ComponentId(2);

/// 命名空间前缀：会话条目（用户消息、助手消息、压缩摘要……）。
const NS_ENTRY: u8 = 1;
/// 命名空间前缀：工具调用块。
const NS_TOOL_CALL: u8 = 2;

/// FNV-1a，够用即可：只用于组件缓存键，不涉及安全场景。
fn fnv1a(prefix: u8, key: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in std::iter::once(prefix).chain(key.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// 由条目 id 派生的组件 id。
pub(crate) fn entry_component_id(entry: &str) -> ComponentId {
    ComponentId(fnv1a(NS_ENTRY, entry).saturating_add(RESERVED_COMPONENT_IDS))
}

/// 由工具调用 id 派生的组件 id。
pub(crate) fn tool_component_id(call_id: &str) -> ComponentId {
    ComponentId(fnv1a(NS_TOOL_CALL, call_id).saturating_add(RESERVED_COMPONENT_IDS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_key_same_namespace_is_stable() {
        assert_eq!(entry_component_id("abc"), entry_component_id("abc"));
        assert_eq!(tool_component_id("abc"), tool_component_id("abc"));
    }

    #[test]
    fn different_namespaces_do_not_collide_for_the_same_key() {
        assert_ne!(entry_component_id("abc"), tool_component_id("abc"));
    }

    #[test]
    fn reserved_range_is_never_produced_by_hashing() {
        for key in ["", "a", "abc", "会话id"] {
            assert!(entry_component_id(key).0 >= RESERVED_COMPONENT_IDS);
            assert!(tool_component_id(key).0 >= RESERVED_COMPONENT_IDS);
        }
    }
}
