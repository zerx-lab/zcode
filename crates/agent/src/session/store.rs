//! 内存条目树 + JSONL 落盘。
//!
//! # 为什么是树
//!
//! 分支（`/rewind`、`/branch`、重试）只需要把 `head` 指到另一条已存在的条目，旧分支
//! 一行不动、零拷贝。线性方案要做到同样的语义就得复制整个会话文件。抄源 oh-my-pi
//! `packages/coding-agent/src/session/session-entries.ts:58-62,253-269`（条目形状：
//! `id`/`parentId`/`timestamp` 三元组撑起整棵树）与
//! `session-manager.ts:201-292`（`SessionEntryIndex`：`entriesById`/`children` 双索引、
//! `insert()` 边追加边建索引、`branch()` 自叶向根回溯再反转）。
//!
//! # 代价
//!
//! - 打开文件必须重放全部条目重建 `id -> 条目` 与 `parent -> children` 两张索引，
//!   不是 O(1) 打开。
//! - [`SessionTree::context`] 是一次自 head 向 root 的回溯（[`SessionTree::branch`]）
//!   加反转，每次调用都重算、不缓存——会话内条目通常是几百到几千条，这个代价可以
//!   接受，换来的是分支永远不需要写放大或复制整个文件。
//!
//! # 读取容错
//!
//! 解不开的 JSONL 行只跳过并 `tracing::warn!`，不中断加载；引用了不存在父条目的行
//! 同样跳过并告警。理由抄源 jcode `crates/jcode-base/src/session/persistence.rs:65-129`
//! （`replay_journal_lines`）：老实现"首错即停"，崩溃或磁盘写一半留下的坏行会让用户
//! 丢失整个尾部历史，而多数坏行只影响它自己那一条，其余行仍然完好。
//!
//! # 压缩切点的安全性
//!
//! [`SessionTree::context`] 把最后一条 [`EntryKind::Compaction`] 之前的消息切成
//! "被摘要吞掉的前缀"与"保留原文的后缀"；若切点会让保留段里出现找不到匹配
//! `ToolCall` 的 `ToolResult`（提供商硬约束，见 [`crate::session::message`] 模块文档
//! 的"不变量"一节），就沿用 jcode `crates/jcode-compaction-core/src/lib.rs:238-291`
//! （`safe_compaction_cutoff`）的思路，把切点逐条前移直到配对补齐，最坏情况回退到
//! 整条路径全部保留（不摘要）——宁可少省 token，也不能让后续每一次请求都因为孤儿
//! `tool_use`/`tool_result` 而 400。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;
use tracing::warn;

use crate::error::StoreError;
use crate::id::{EntryId, SessionId};
use crate::session::entry::{EntryKind, SessionEntry};
use crate::session::message::{MessageRecord, StoredAssistantContent, StoredMessage};

/// 内存中的会话条目树。
///
/// 两张索引撑起整棵树：`entries` 是 `id -> 条目`，`children` 是 `id -> 子条目 id 列表`。
/// `head` 是当前叶子；"当前上下文"永远是根到 `head` 的路径，`/rewind`、`/branch` 只是
/// 把 `head` 改指向另一条已存在的条目。
#[derive(Debug)]
pub struct SessionTree {
    session_id: SessionId,
    root_id: EntryId,
    entries: HashMap<EntryId, SessionEntry>,
    children: HashMap<EntryId, Vec<EntryId>>,
    head: EntryId,
}

impl SessionTree {
    /// 用根条目（必须是 [`EntryKind::SessionInit`]）建树。
    #[must_use]
    pub fn new(session_id: SessionId, cwd: String, model: String) -> Self {
        let root = SessionEntry::new(None, EntryKind::SessionInit { cwd, model });
        let root_id = root.id.clone();
        let mut entries = HashMap::new();
        entries.insert(root_id.clone(), root);
        Self {
            session_id,
            root_id: root_id.clone(),
            entries,
            children: HashMap::new(),
            head: root_id,
        }
    }

    /// 从一组条目重建整棵树。
    ///
    /// 条目按 id 排序后重放：字典序即时间序（见 [`crate::id`] 模块文档），排序后父
    /// 条目必然先于子条目出现，[`SessionTree::insert`] 不需要多趟扫描就能建好索引。
    /// head 取重放后"最晚的叶子"——叶子集合（`children` 里查不到自己的条目）里 id
    /// 最大的那个，对应 oh-my-pi `session-manager.ts:283-292` 的 `branch()` 回溯
    /// 起点：那边靠"最后一次 `insert` 设置的游标"记住最新叶子，本实现改成显式取
    /// 最大 id，效果等价但不依赖调用方的传入顺序。
    ///
    /// 悬空父引用（父条目不存在）与重复根一律跳过并 `tracing::warn!`；没有任何合法
    /// 根才是 [`StoreError::MissingRoot`]——理由见模块文档"读取容错"一节。
    ///
    /// 这里返回的 `MissingRoot.path` 是占位空路径：本方法不知道真实文件路径，
    /// 知道路径的 [`SessionStore::open`] 会在收到这个错误后原样替换成真实路径。
    pub fn from_entries(
        session_id: SessionId,
        entries: Vec<SessionEntry>,
    ) -> Result<Self, StoreError> {
        let mut entries = entries;
        entries.sort_by(|a, b| a.id.cmp(&b.id));

        let Some(root_pos) = entries.iter().position(|entry| entry.parent_id.is_none()) else {
            return Err(StoreError::MissingRoot {
                path: PathBuf::new(),
            });
        };
        let root = entries.remove(root_pos);
        if !matches!(root.kind, EntryKind::SessionInit { .. }) {
            return Err(StoreError::MissingRoot {
                path: PathBuf::new(),
            });
        }

        let root_id = root.id.clone();
        let mut tree = Self {
            session_id,
            root_id: root_id.clone(),
            entries: HashMap::new(),
            children: HashMap::new(),
            head: root_id.clone(),
        };
        tree.entries.insert(root_id, root);

        for entry in entries {
            if entry.parent_id.is_none() {
                warn!(entry_id = %entry.id, "跳过重复的根条目");
                continue;
            }
            let child_id = entry.id.clone();
            if let Err(error) = tree.insert(entry) {
                warn!(entry_id = %child_id, %error, "跳过悬空父引用的条目");
            }
        }

        tree.head = tree.latest_leaf();
        Ok(tree)
    }

    /// 会话 id。
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// 当前叶子。
    #[must_use]
    pub fn head(&self) -> &EntryId {
        &self.head
    }

    /// 切到另一个条目作为 head（`/rewind`、`/branch` 就是这一个动作）。
    ///
    /// 条目不存在时返回 `false` 且不改动 head。
    #[must_use]
    pub fn set_head(&mut self, id: &EntryId) -> bool {
        if self.entries.contains_key(id) {
            self.head = id.clone();
            true
        } else {
            false
        }
    }

    /// 在 head 下面追加一条自动生成 id 的条目，等价于
    /// `append_with_id(EntryId::generate(), kind)`。
    #[must_use]
    pub fn append(&mut self, kind: EntryKind) -> SessionEntry {
        self.append_with_id(EntryId::generate(), kind)
    }

    /// 用调用方给定的 `id` 追加一条条目，而不是内部生成。
    ///
    /// turn 循环需要在开流之前就确定助手消息的条目 id——先发 `MessageStart { entry }`
    /// 事件，随后的 `TextDelta`/`ToolCallDelta` 才能按这个 id 归属到正确的消息；
    /// 落盘只会在整条流结束之后发生。所以 id 的生成（早）和条目的落地（晚）必须
    /// 拆成两步，[`SessionTree::append`] 内部生成 id 那一步单独抽成本方法。
    ///
    /// **重复 `id` 的语义是覆盖，不是报错**：若 `id` 已经在树里，本方法只替换那条
    /// 已有条目的 `kind`（并刷新时间戳），不改动它的 `parent_id`、不移动 `head`、
    /// 也不会往 `children` 里再插一条——这样即使调用方因为重试而传了同一个 id 两次，
    /// 树的父子索引也不会出现"同一个 id 挂在两个位置"的悬空引用。选覆盖而不是报错，
    /// 是因为返回类型是 `SessionEntry` 而非 `Result`：这条 API 服务的是流式写入路径，
    /// 调用方在拿到 `SessionEntry` 之前没法回滚已经发给客户端的事件，报错没地方接。
    #[must_use]
    pub fn append_with_id(&mut self, id: EntryId, kind: EntryKind) -> SessionEntry {
        if let Some(existing) = self.entries.get_mut(&id) {
            existing.kind = kind;
            existing.timestamp_ms = crate::id::now_millis();
            return existing.clone();
        }

        let entry = SessionEntry {
            id: id.clone(),
            parent_id: Some(self.head.clone()),
            timestamp_ms: crate::id::now_millis(),
            kind,
        };
        self.children
            .entry(self.head.clone())
            .or_default()
            .push(id.clone());
        self.entries.insert(id.clone(), entry.clone());
        self.head = id;
        entry
    }

    /// 把一条已有条目接回树上——用于 [`SessionTree::from_entries`] 重放文件里的条目。
    ///
    /// 父条目必须已经在树里；找不到就返回 [`StoreError::DanglingParent`]，跳过并
    /// 告警还是向上传播由调用方决定（见模块文档"读取容错"）。
    pub fn insert(&mut self, entry: SessionEntry) -> Result<(), StoreError> {
        // 同 id 重放：只覆盖内容，绝不再挂一次 child。JSONL 是追加文件，
        // 同一个 id 出现两行是可能的（`append_with_id` 的覆盖语义），
        // 重复挂 child 会让 `branch()` 的父子索引出现重复分叉。
        if self.entries.contains_key(&entry.id) {
            self.entries.insert(entry.id.clone(), entry);
            return Ok(());
        }
        match &entry.parent_id {
            Some(parent) => {
                if !self.entries.contains_key(parent) {
                    return Err(StoreError::DanglingParent {
                        child: entry.id.clone(),
                        parent: parent.clone(),
                    });
                }
                self.children
                    .entry(parent.clone())
                    .or_default()
                    .push(entry.id.clone());
            }
            // 树已经有根时，另一条声称自己是根的条目无处可挂——按悬空父引用同样的
            // 路径处理：它缺的不是某个具体父条目，而是"任何"父条目，用自身 id 占位。
            None if !self.entries.is_empty() => {
                return Err(StoreError::DanglingParent {
                    child: entry.id.clone(),
                    parent: entry.id.clone(),
                });
            }
            None => {}
        }
        self.entries.insert(entry.id.clone(), entry);
        Ok(())
    }

    /// 根到 head 的路径，按时间顺序（根在前，head 在后）。
    #[must_use]
    pub fn branch(&self) -> Vec<&SessionEntry> {
        let mut path = Vec::new();
        let mut current = Some(&self.head);
        while let Some(id) = current {
            let Some(entry) = self.entries.get(id) else {
                break;
            };
            path.push(entry);
            current = entry.parent_id.as_ref();
        }
        path.reverse();
        path
    }

    /// 当前标题：路径上最后一条 [`EntryKind::TitleChange`] 生效。
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.branch()
            .into_iter()
            .rev()
            .find_map(|entry| match &entry.kind {
                EntryKind::TitleChange { title } => Some(title.as_str()),
                EntryKind::SessionInit { .. }
                | EntryKind::Message { .. }
                | EntryKind::ModelChange { .. }
                | EntryKind::Compaction { .. } => None,
            })
    }

    /// 当前模型：路径上最后一条 [`EntryKind::ModelChange`]，否则根条目
    /// （[`EntryKind::SessionInit`]）里记录的初始模型。
    #[must_use]
    pub fn model(&self) -> &str {
        for entry in self.branch().into_iter().rev() {
            if let EntryKind::ModelChange { model } = &entry.kind {
                return model;
            }
        }
        self.entries
            .get(&self.root_id)
            .and_then(|entry| match &entry.kind {
                EntryKind::SessionInit { model, .. } => Some(model.as_str()),
                EntryKind::Message { .. }
                | EntryKind::ModelChange { .. }
                | EntryKind::TitleChange { .. }
                | EntryKind::Compaction { .. } => None,
            })
            .unwrap_or_default()
    }

    /// 会话建立时的工作目录（根条目里记录的那个，不随时间变化）。
    #[must_use]
    pub fn cwd(&self) -> &str {
        self.entries
            .get(&self.root_id)
            .and_then(|entry| match &entry.kind {
                EntryKind::SessionInit { cwd, .. } => Some(cwd.as_str()),
                EntryKind::Message { .. }
                | EntryKind::ModelChange { .. }
                | EntryKind::TitleChange { .. }
                | EntryKind::Compaction { .. } => None,
            })
            .unwrap_or_default()
    }

    /// 当前上下文：路径上的消息，已应用压缩。见模块文档"压缩切点的安全性"一节。
    #[must_use]
    pub fn context(&self) -> Vec<MessageRecord> {
        let path = self.branch();

        // 找路径上最后一条 Compaction；同时借出它需要的三样东西，避免之后再按下标
        // 回查 `path`（`path[idx]` 会撞上 `indexing_slicing` 这条 deny lint）。
        let mut compaction: Option<(usize, &EntryId, &str, Option<&EntryId>)> = None;
        for (idx, entry) in path.iter().enumerate() {
            if let EntryKind::Compaction {
                summary,
                first_kept,
                ..
            } = &entry.kind
            {
                compaction = Some((idx, &entry.id, summary.as_str(), first_kept.as_ref()));
            }
        }

        let Some((split_idx, comp_id, summary, first_kept)) = compaction else {
            return path
                .into_iter()
                .filter_map(|entry| {
                    entry.message().map(|message| MessageRecord {
                        id: entry.id.clone(),
                        message: message.clone(),
                    })
                })
                .collect();
        };

        // 压缩条目之前的消息可能被摘要吞掉；之后的消息（压缩发生后继续的对话）永远
        // 原样保留，不受切点影响。
        let mut pre = Vec::new();
        let mut post = Vec::new();
        for (idx, entry) in path.iter().enumerate() {
            let Some(message) = entry.message() else {
                continue;
            };
            let record = MessageRecord {
                id: entry.id.clone(),
                message: message.clone(),
            };
            match idx.cmp(&split_idx) {
                std::cmp::Ordering::Less => pre.push(record),
                std::cmp::Ordering::Greater => post.push(record),
                std::cmp::Ordering::Equal => {}
            }
        }

        let naive_k = first_kept.map_or(pre.len(), |id| {
            pre.partition_point(|record| &record.id < id)
        });
        let safe_k = safe_prefix_cutoff(&pre, &post, naive_k);

        let kept_pre = pre.split_off(safe_k);
        let dropped_pre = pre;

        let mut out = Vec::with_capacity(1 + kept_pre.len() + post.len());
        if !dropped_pre.is_empty() {
            out.push(MessageRecord {
                id: comp_id.clone(),
                message: StoredMessage::system_reminder(summary),
            });
        }
        out.extend(kept_pre);
        out.extend(post);
        out
    }

    /// 叶子集合（`children` 里查不到自己的条目）里 id 最大的那个，即"最晚"的叶子。
    fn latest_leaf(&self) -> EntryId {
        self.entries
            .keys()
            .filter(|id| self.children.get(*id).is_none_or(Vec::is_empty))
            .max()
            .cloned()
            .unwrap_or_else(|| self.root_id.clone())
    }
}

/// 把 `record` 里的 `ToolCall`/`ToolResult` id 计入 `available`/`missing`。
///
/// `missing` 只在扫到 `ToolResult` 而其 `tool_call_id` 还没在 `available` 里出现过
/// 时才记；后续（无论是继续向后扫，还是 [`safe_prefix_cutoff`] 里向前回溯）扫到匹配
/// 的 `ToolCall` 就把它移出 `missing`。
fn scan_tool_ids<'a>(
    record: &'a MessageRecord,
    available: &mut HashSet<&'a str>,
    missing: &mut HashSet<&'a str>,
) {
    match &record.message {
        StoredMessage::Assistant { content, .. } => {
            for block in content {
                if let StoredAssistantContent::ToolCall { id, .. } = block {
                    available.insert(id.as_str());
                    missing.remove(id.as_str());
                }
            }
        }
        StoredMessage::ToolResult { tool_call_id, .. } => {
            if !available.contains(tool_call_id.as_str()) {
                missing.insert(tool_call_id.as_str());
            }
        }
        StoredMessage::User { .. } => {}
    }
}

/// 计算安全的压缩切点：从 `naive_k`（`pre` 里保留段的起点）出发，若保留段
/// （`pre[naive_k..]` 拼上 `post`）里有 `ToolResult` 找不到同样保留的 `ToolCall`，
/// 就把切点前移一条（也就是多保留一条原文），直到配对补齐或者退到 0（全部保留，
/// 相当于放弃这次摘要）。思路抄自 jcode
/// `crates/jcode-compaction-core/src/lib.rs:238-291`（`safe_compaction_cutoff`），
/// 用 `pre`/`post` 两段而不是单个 `messages` 数组，是因为本仓的"压缩之后的对话"
/// （`post`）在语义上永远不参与摘要取舍，不需要也不应该被回溯到。
fn safe_prefix_cutoff(pre: &[MessageRecord], post: &[MessageRecord], naive_k: usize) -> usize {
    let mut available = HashSet::new();
    let mut missing = HashSet::new();

    for record in pre.get(naive_k..).unwrap_or(&[]).iter().chain(post) {
        scan_tool_ids(record, &mut available, &mut missing);
    }

    let mut k = naive_k;
    while !missing.is_empty() {
        let Some(prev) = k.checked_sub(1) else {
            break;
        };
        let Some(record) = pre.get(prev) else {
            break;
        };
        scan_tool_ids(record, &mut available, &mut missing);
        k = prev;
    }
    k
}

/// 文件支持的会话存储：内存树 + 一份持续打开、只追加的 JSONL 文件。
///
/// [`SessionStore::append`] 的顺序固定为"先进内存树、再写一行、再 `flush`"：写入
/// 失败时内存树已经领先于磁盘，调用方应当整体丢弃这次 `SessionStore`（下次
/// [`SessionStore::open`] 会从磁盘上实际落地的内容重建，不会看到这条丢失的条目）。
#[derive(Debug)]
pub struct SessionStore {
    tree: SessionTree,
    path: PathBuf,
    file: tokio::fs::File,
}

impl SessionStore {
    /// 在 `dir` 下新建一个会话文件（文件名 = `<session_id>.jsonl`）并写入根条目。
    pub async fn create(dir: &Path, cwd: String, model: String) -> Result<Self, StoreError> {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|source| StoreError::Io {
                path: dir.to_path_buf(),
                source,
            })?;

        let session_id = SessionId::generate();
        let path = dir.join(format!("{session_id}.jsonl"));
        let root = SessionEntry::new(None, EntryKind::SessionInit { cwd, model });

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|source| StoreError::Io {
                path: path.clone(),
                source,
            })?;
        write_line(&mut file, &path, &root).await?;

        // `from_entries` 只会在"找不到合法根"时失败；这里刚构造并写入了一条
        // `SessionInit` 根条目，不会走到那个分支——失败时按真实存储故障上抛即可。
        let tree = SessionTree::from_entries(session_id, vec![root])
            .map_err(|error| retarget_missing_root(error, &path))?;

        Ok(Self { tree, path, file })
    }

    /// 打开已有会话文件。
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let session_id = session_id_from_path(path);

        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|source| StoreError::Io {
                path: path.to_path_buf(),
                source,
            })?;

        let mut entries = Vec::new();
        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<SessionEntry>(trimmed) {
                Ok(entry) => entries.push(entry),
                Err(error) => {
                    warn!(
                        path = %path.display(),
                        line = line_no + 1,
                        %error,
                        "跳过无法解析的会话条目行"
                    );
                }
            }
        }

        let tree = SessionTree::from_entries(session_id, entries)
            .map_err(|error| retarget_missing_root(error, path))?;

        let file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .await
            .map_err(|source| StoreError::Io {
                path: path.to_path_buf(),
                source,
            })?;

        Ok(Self {
            tree,
            path: path.to_path_buf(),
            file,
        })
    }

    /// 借出内存条目树（只读）。
    #[must_use]
    pub fn tree(&self) -> &SessionTree {
        &self.tree
    }

    /// 借出内存条目树（可变）——`set_head` 之类不产生新条目的操作走这条路径。
    #[must_use]
    pub fn tree_mut(&mut self) -> &mut SessionTree {
        &mut self.tree
    }

    /// 会话文件路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 追加一条条目：先进内存树，再追写一行 JSONL 并 `flush`。返回新条目 id。
    ///
    /// 等价于用 `EntryId::generate()` 生成一个 id，再 [`SessionStore::append_with_id`]。
    pub async fn append(&mut self, kind: EntryKind) -> Result<EntryId, StoreError> {
        let id = EntryId::generate();
        self.append_with_id(id.clone(), kind).await?;
        Ok(id)
    }

    /// 用调用方给定的 `id` 追加一条条目：先进内存树，再追写一行 JSONL 并 `flush`。
    ///
    /// 语义见 [`SessionTree::append_with_id`]——turn 循环靠它把"流开始前就已经发给
    /// 客户端的条目 id"落到磁盘，流结束后才调用一次；重复传同一个 `id` 是覆盖内存
    /// 树里那条条目，但仍会在 JSONL 里追加一行新记录（只追加、绝不回改旧行），
    /// 重放时后一行的内容覆盖前一行，树的父子结构不受影响。
    pub async fn append_with_id(&mut self, id: EntryId, kind: EntryKind) -> Result<(), StoreError> {
        let entry = self.tree.append_with_id(id, kind);
        write_line(&mut self.file, &self.path, &entry).await
    }
}

/// 序列化一条条目、追加写入一行并立即 `flush`——晚一步 flush 就可能在崩溃时丢掉刚
/// 写的这一行,同时让内存树领先于磁盘。
async fn write_line(
    file: &mut tokio::fs::File,
    path: &Path,
    entry: &SessionEntry,
) -> Result<(), StoreError> {
    let mut line = serde_json::to_string(entry).map_err(StoreError::Encode)?;
    line.push('\n');
    file.write_all(line.as_bytes())
        .await
        .map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.flush().await.map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// 把 [`SessionTree::from_entries`] 返回的占位空路径换成真实文件路径。
fn retarget_missing_root(error: StoreError, path: &Path) -> StoreError {
    match error {
        StoreError::MissingRoot { .. } => StoreError::MissingRoot {
            path: path.to_path_buf(),
        },
        other => other,
    }
}

/// 会话文件名去掉 `.jsonl` 后缀就是它的会话 id；取不到（非法 UTF-8 文件名等极端
/// 情况）时退化为生成一个新 id——不影响读出来的树，只影响 `SessionTree::session_id()`
/// 这一个访问器的返回值。
fn session_id_from_path(path: &Path) -> SessionId {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map_or_else(SessionId::generate, |stem| SessionId::from(stem.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use tempfile::tempdir;

    use super::*;
    use crate::session::entry::CompactionReason;
    use crate::session::message::{
        DisplayRole, StoredStopReason, StoredToolResultContent, StoredUsage, StoredUserContent,
    };

    fn user_message(text: &str) -> EntryKind {
        EntryKind::Message {
            message: StoredMessage::user(text),
        }
    }

    fn assistant_text(text: &str) -> EntryKind {
        EntryKind::Message {
            message: StoredMessage::Assistant {
                content: vec![StoredAssistantContent::Text {
                    text: text.to_owned(),
                }],
                model: None,
                usage: StoredUsage::default(),
                stop_reason: StoredStopReason::default(),
            },
        }
    }

    fn assistant_tool_call(call_id: &str) -> EntryKind {
        EntryKind::Message {
            message: StoredMessage::Assistant {
                content: vec![StoredAssistantContent::ToolCall {
                    id: call_id.to_owned(),
                    name: "read".to_owned(),
                    arguments: "{}".to_owned(),
                }],
                model: None,
                usage: StoredUsage::default(),
                stop_reason: StoredStopReason::default(),
            },
        }
    }

    fn tool_result(call_id: &str) -> EntryKind {
        EntryKind::Message {
            message: StoredMessage::ToolResult {
                tool_call_id: call_id.to_owned(),
                tool_name: "read".to_owned(),
                content: vec![StoredToolResultContent::Text {
                    text: "内容".to_owned(),
                }],
                is_error: false,
            },
        }
    }

    #[tokio::test]
    async fn reopen_reproduces_tree_and_context() {
        let dir = tempdir().expect("创建临时目录");
        let mut store = SessionStore::create(dir.path(), "/repo".to_owned(), "gpt-5".to_owned())
            .await
            .expect("创建会话");
        store
            .append(user_message("你好"))
            .await
            .expect("追加用户消息");
        store
            .append(assistant_text("你好呀"))
            .await
            .expect("追加助手消息");

        let expected_head = store.tree().head().clone();
        let expected_context = store.tree().context();
        let path = store.path().to_path_buf();
        drop(store);

        let reopened = SessionStore::open(&path).await.expect("重新打开会话文件");
        assert_eq!(
            reopened.tree().head(),
            &expected_head,
            "重放后 head 必须落在同一条叶子上"
        );
        assert_eq!(
            reopened.tree().context(),
            expected_context,
            "重放后的上下文必须和落盘前完全一致"
        );
    }

    #[tokio::test]
    async fn set_head_to_an_earlier_entry_forks_and_keeps_the_old_branch_on_disk() {
        let dir = tempdir().expect("创建临时目录");
        let mut store = SessionStore::create(dir.path(), "/repo".to_owned(), "gpt-5".to_owned())
            .await
            .expect("创建会话");
        let root_id = store.tree().head().clone();

        let first = store
            .append(user_message("第一条"))
            .await
            .expect("追加第一条");
        store
            .append(user_message("第二条，将被分支绕过"))
            .await
            .expect("追加第二条");

        assert!(
            store.tree_mut().set_head(&first),
            "head 必须能切回已存在的条目"
        );
        let branch_entry = store
            .append(user_message("分支出的第三条"))
            .await
            .expect("在分支上追加");

        let ids: Vec<EntryId> = store
            .tree()
            .branch()
            .into_iter()
            .map(|entry| entry.id.clone())
            .collect();
        assert_eq!(
            ids,
            vec![root_id, first, branch_entry],
            "新分支应当是 根 -> first -> branch_entry，跳过被绕开的第二条"
        );

        let path = store.path().to_path_buf();
        drop(store);
        let raw = tokio::fs::read_to_string(&path)
            .await
            .expect("读取原始文件");
        assert_eq!(
            raw.lines().count(),
            4,
            "根 + 三条追加 = 4 行，旧分支的第二条不应被截断或覆盖"
        );
    }

    #[tokio::test]
    async fn corrupt_line_is_skipped_without_losing_the_rest() {
        let dir = tempdir().expect("创建临时目录");
        let mut store = SessionStore::create(dir.path(), "/repo".to_owned(), "gpt-5".to_owned())
            .await
            .expect("创建会话");
        store
            .append(user_message("坏行之前"))
            .await
            .expect("追加坏行之前的消息");
        let path = store.path().to_path_buf();
        drop(store);

        {
            let mut file = tokio::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .await
                .expect("重新以追加模式打开");
            file.write_all(b"{not valid json\n")
                .await
                .expect("写入损坏行");
            file.flush().await.expect("flush 损坏行");
        }

        let mut reopened = SessionStore::open(&path)
            .await
            .expect("即便有坏行也必须能打开");
        reopened
            .append(user_message("坏行之后"))
            .await
            .expect("坏行之后必须仍能正常追加");

        let context = reopened.tree().context();
        assert_eq!(
            context.len(),
            2,
            "坏行本身被跳过，坏行前后的两条正常消息都必须在"
        );
    }

    #[test]
    fn title_and_model_follow_the_latest_change_on_the_path() {
        let mut tree = SessionTree::new(
            SessionId::generate(),
            "/repo".to_owned(),
            "gpt-5".to_owned(),
        );
        assert_eq!(
            tree.model(),
            "gpt-5",
            "没有 ModelChange 时取根条目的初始模型"
        );
        assert_eq!(tree.title(), None, "没有 TitleChange 时没有标题");

        let _ = tree.append(EntryKind::TitleChange {
            title: "第一个标题".to_owned(),
        });
        let _ = tree.append(EntryKind::ModelChange {
            model: "gpt-6".to_owned(),
        });
        let _ = tree.append(EntryKind::TitleChange {
            title: "第二个标题".to_owned(),
        });

        assert_eq!(
            tree.title(),
            Some("第二个标题"),
            "title() 必须取路径上最后一条 TitleChange"
        );
        assert_eq!(
            tree.model(),
            "gpt-6",
            "model() 必须取路径上最后一条 ModelChange"
        );
        assert_eq!(
            tree.cwd(),
            "/repo",
            "cwd 不随时间变化，恒等于根条目里记录的那个"
        );
    }

    #[test]
    fn compaction_replaces_the_summarized_prefix_and_keeps_the_retained_tail() {
        let mut tree = SessionTree::new(
            SessionId::generate(),
            "/repo".to_owned(),
            "gpt-5".to_owned(),
        );

        let _ = tree.append(user_message("问题一"));
        let kept_from = tree.append(user_message("问题二"));
        let _ = tree.append(assistant_text("答案二"));

        let compaction = tree.append(EntryKind::Compaction {
            summary: "早期对话摘要".to_owned(),
            first_kept: Some(kept_from.id.clone()),
            reason: CompactionReason::Threshold,
        });
        let _ = tree.append(user_message("问题三"));

        let context = tree.context();

        // 摘要打头，id 用压缩条目自己的，展示成系统提醒但 API 角色仍是 user。
        let summary = context.first().expect("上下文不能是空的");
        assert_eq!(summary.id, compaction.id);
        match &summary.message {
            StoredMessage::User {
                content,
                display_role,
            } => {
                assert_eq!(display_role, &Some(DisplayRole::System));
                assert_eq!(
                    content,
                    &vec![StoredUserContent::Text {
                        text: "早期对话摘要".to_owned()
                    }]
                );
            }
            other => panic!("摘要必须是 system_reminder 形态的 User 消息，实际是 {other:?}"),
        }

        // “问题一”在切点之前且没有工具配对风险，应当被摘要吞掉。
        assert!(
            !context.iter().any(|record| record.id != compaction.id
                && record.message == StoredMessage::user("问题一")),
            "被摘要吞掉的消息不应再原样出现"
        );
        // 从 first_kept 开始的原文，以及压缩之后新产生的消息，必须原样保留。
        assert!(context.iter().any(|record| record.id == kept_from.id));
        assert!(
            context
                .iter()
                .any(|record| record.message == StoredMessage::user("问题三"))
        );
    }

    #[test]
    fn compaction_cutoff_never_leaves_an_orphaned_tool_call() {
        let mut tree = SessionTree::new(
            SessionId::generate(),
            "/repo".to_owned(),
            "gpt-5".to_owned(),
        );

        let _ = tree.append(user_message("问题一"));
        let call_entry = tree.append(assistant_tool_call("call1"));
        let result_entry = tree.append(tool_result("call1"));
        let _ = tree.append(user_message("问题二"));
        let _ = tree.append(assistant_text("答案二"));

        // 切点卡在 ToolCall 和它的 ToolResult 中间：朴素实现会把 ToolCall 那条摘要
        // 掉，只留下引用不到调用的 ToolResult。
        let compaction = tree.append(EntryKind::Compaction {
            summary: "早期对话摘要".to_owned(),
            first_kept: Some(result_entry.id.clone()),
            reason: CompactionReason::Threshold,
        });
        let _ = tree.append(user_message("问题三"));

        let context = tree.context();

        let mut call_ids = HashSet::new();
        let mut result_ids = HashSet::new();
        for record in &context {
            match &record.message {
                StoredMessage::Assistant { content, .. } => {
                    for block in content {
                        if let StoredAssistantContent::ToolCall { id, .. } = block {
                            call_ids.insert(id.clone());
                        }
                    }
                }
                StoredMessage::ToolResult { tool_call_id, .. } => {
                    result_ids.insert(tool_call_id.clone());
                }
                StoredMessage::User { .. } => {}
            }
        }
        assert_eq!(
            call_ids, result_ids,
            "context() 绝不能留下没有配对的 ToolCall/ToolResult"
        );

        // 安全切点必须往前推到把 ToolCall 也保住，而不是干脆整体不摘要。
        assert!(
            context.iter().any(|record| record.id == call_entry.id),
            "为了不留孤儿，ToolCall 所在的消息必须被保留"
        );
        assert_eq!(
            context
                .iter()
                .filter(|record| record.id == compaction.id)
                .count(),
            1,
            "摘要仍然应当以压缩条目自己的 id 出现恰好一次"
        );
    }

    #[test]
    fn append_with_id_overwrites_in_place_on_a_repeated_id() {
        let mut tree = SessionTree::new(
            SessionId::generate(),
            "/repo".to_owned(),
            "gpt-5".to_owned(),
        );
        let msg_id = EntryId::generate();

        let first = tree.append_with_id(msg_id.clone(), assistant_text("流式增量还没写完"));
        assert_eq!(
            tree.head(),
            &msg_id,
            "第一次调用要像 append 一样把 head 前移到新条目"
        );

        let second = tree.append_with_id(msg_id.clone(), assistant_text("流式增量已经写完"));

        // 覆盖语义：id、parent_id 不变，head 不再移动，树也不会多长出一层。
        assert_eq!(second.id, first.id);
        assert_eq!(second.parent_id, first.parent_id);
        assert_eq!(tree.head(), &msg_id, "覆盖不应该移动 head");
        assert_eq!(
            tree.branch().len(),
            2,
            "覆盖不应该在树里多插一层——根 + 这一条消息，仍然是两层"
        );

        match second.message().expect("消息条目必有消息体") {
            StoredMessage::Assistant { content, .. } => match content.first().expect("有内容块")
            {
                StoredAssistantContent::Text { text } => {
                    assert_eq!(text, "流式增量已经写完", "第二次调用的内容必须覆盖第一次");
                }
                other => panic!("期望文本内容，实际是 {other:?}"),
            },
            other => panic!("期望助手消息，实际是 {other:?}"),
        }
    }
}
