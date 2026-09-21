//! 收藏仓库的数据模型。

use serde::{Deserialize, Serialize};

/// 仓库的稳定身份：`owner/repo` 的小写形式。
///
/// 排序、去重、编辑都作用在它上面；展示时才回到 `Repo::full_name` 的原始大小写。
pub type RepoId = String;

/// 一个被管理的仓库。
///
/// 这是序列化记录（写进 `library.json`），字段公开是为了让 JSON 结构一眼可读；
/// 行为（去重、排序、元数据补全）由 `Library` 统一持有。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    /// `owner/repo`，保留 GitHub 上的原始大小写。
    pub full_name: String,
    /// 仓库地址，例如 `https://github.com/coder/coder`。
    pub url: String,
    /// About 的原文，来自仓库页面。
    #[serde(default)]
    pub description: Option<String>,
    /// 人工填写的汉语描述。留空表示尚未填写。
    #[serde(default)]
    pub description_zh: Option<String>,
    /// 发现时间（即入库时间），Unix 秒。
    pub discovered_at: i64,
    /// 仓库创建时间，Unix 秒。需要 GitHub 接口，可能暂时为空。
    #[serde(default)]
    pub created_at: Option<i64>,
    /// 发现来源的名称；手工添加时为 `None`。
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub stars: Option<u64>,
    /// 所属分类。一个仓库最多属于一个分类，`None` 表示未分类。
    ///
    /// 这里存的是词表里的名字本身，不是 id：`library.json` 直接手看就能读懂。
    /// 改分类名时由 `Library::rename_name` 负责把引用一起改掉。
    #[serde(default)]
    pub category: Option<String>,
    /// 标签，可以有任意多个。同样存名字，去重与顺序都由 `Library` 保证。
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Repo {
    /// 手工添加时使用：描述与创建时间留空，等元数据补全步骤去填。
    pub fn new(full_name: String, source: Option<String>, discovered_at: i64) -> Self {
        Self {
            url: format!("https://github.com/{full_name}"),
            full_name,
            description: None,
            description_zh: None,
            discovered_at,
            created_at: None,
            source,
            language: None,
            stars: None,
            category: None,
            tags: Vec::new(),
        }
    }

    pub fn id(&self) -> RepoId {
        self.full_name.to_lowercase()
    }

    /// 列表里显示的一行描述：汉语优先，其次原文。
    pub fn display_description(&self) -> Option<&str> {
        clean_text(self.description_zh.as_deref())
            .or_else(|| clean_text(self.description.as_deref()))
    }

    /// 还有字段没取到，需要再次访问 GitHub。
    pub fn has_pending_metadata(&self) -> bool {
        clean_text(self.description.as_deref()).is_none() || self.created_at.is_none()
    }
}

/// 把 `owner/repo`、`https://github.com/owner/repo`、`github.com/owner/repo.git`
/// 之类的输入统一成 `owner/repo`。
pub fn parse_full_name(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let path = without_scheme
        .strip_prefix("github.com/")
        .unwrap_or(without_scheme);

    let mut parts = path.trim_matches('/').split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if parts.next().is_some() {
        // 多出来的段（/tree/main 之类）不算仓库标识。
        return None;
    }

    let name = name.strip_suffix(".git").unwrap_or(name);
    if !is_repo_segment(owner) || !is_repo_segment(name) {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

fn is_repo_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// 人工输入里常见全角空格与多余换行，统一收干净。
fn clean_text(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_forms_people_actually_paste() {
        for input in [
            "coder/coder",
            "  coder/coder  ",
            "https://github.com/coder/coder",
            "http://github.com/coder/coder/",
            "github.com/coder/coder",
            "https://github.com/coder/coder.git",
        ] {
            assert_eq!(
                parse_full_name(input).as_deref(),
                Some("coder/coder"),
                "输入：{input}"
            );
        }
    }

    #[test]
    fn rejects_input_that_is_not_a_repository() {
        for input in [
            "",
            "coder",
            "coder/coder/tree/main",
            "https://gitlab.com/coder/coder",
            "https://github.com/",
        ] {
            assert_eq!(parse_full_name(input), None, "输入：{input}");
        }
    }

    #[test]
    fn id_is_case_insensitive_so_the_same_repo_never_lands_twice() {
        let upper = Repo::new("Coder/Coder".to_string(), None, 0);
        let lower = Repo::new("coder/coder".to_string(), None, 0);
        assert_eq!(upper.id(), lower.id());
    }

    #[test]
    fn display_description_prefers_the_manual_chinese_text() {
        let mut repo = Repo::new("coder/coder".to_string(), None, 0);
        assert_eq!(repo.display_description(), None);

        repo.description = Some("原文".into());
        assert_eq!(repo.display_description(), Some("原文"));

        repo.description_zh = Some("  汉语  ".into());
        assert_eq!(repo.display_description(), Some("汉语"));

        // 只有空白的手工描述等同于没填。
        repo.description_zh = Some("   ".into());
        assert_eq!(repo.display_description(), Some("原文"));
    }

    #[test]
    fn pending_metadata_tracks_both_missing_fields() {
        let mut repo = Repo::new("coder/coder".to_string(), None, 0);
        assert!(repo.has_pending_metadata());

        repo.description = Some("描述".into());
        assert!(repo.has_pending_metadata(), "创建时间仍然缺失");

        repo.created_at = Some(1);
        assert!(!repo.has_pending_metadata());
    }

    #[test]
    fn a_repo_without_category_or_tags_still_parses() {
        // 加分类标签之前写下的 library.json 里没有这两个字段，读它不能失败。
        let json = r#"{
            "full_name": "coder/coder",
            "url": "https://github.com/coder/coder",
            "discovered_at": 7
        }"#;
        let repo: Repo = serde_json::from_str(json).unwrap();

        assert_eq!(repo.category, None);
        assert!(repo.tags.is_empty());
        assert_eq!(repo.full_name, "coder/coder");
    }

    #[test]
    fn category_and_tags_round_trip_through_json() {
        let mut repo = Repo::new("coder/coder".to_string(), None, 1);
        repo.category = Some("开发工具".to_string());
        repo.tags = vec!["cli".to_string(), "rust".to_string()];

        let text = serde_json::to_string(&repo).unwrap();
        assert_eq!(serde_json::from_str::<Repo>(&text).unwrap(), repo);
    }
}
