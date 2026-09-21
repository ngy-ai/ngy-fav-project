//! 分类与标签的纯逻辑。
//!
//! 两者共用同一套「名字」规则：名字就是身份，比较时忽略大小写，存下来的是第一次
//! 出现的写法。区别在归属——一个仓库最多属于一个分类，可以挂任意多个标签。
//!
//! **分类是一棵树**，树本身在 [`super::tree`] 里；这里只管名字和路径语法：
//! 用户输入 `开发/前端` 时怎么把这条路径收干净，以及从路径里切出各级名字。
//! 树上的父子关系不从这里推断——那是 `parent_id` 的事。
//!
//! 标签是扁平的，所以只需要名字规则，外加一个稳定的 id（改标签名时不必回头改
//! 仓库上的一堆引用）。

/// 名字的长度上限。它是一行给人看的字，超过这个长度一定是误输入。
pub const MAX_NAME_CHARS: usize = 40;

/// 分类路径的分隔符。全角 `／` 在清洗时会被换成它。
pub const PATH_SEPARATOR: char = '/';

/// 分类最多几级。再深就不是「归类」而是「重新发明目录树」了。
pub const MAX_DEPTH: usize = 6;

/// 词表的两个种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaxonomyKind {
    Category,
    Tag,
}

impl TaxonomyKind {
    /// 界面上称呼它的词。
    pub fn label(self) -> &'static str {
        match self {
            Self::Category => "分类",
            Self::Tag => "标签",
        }
    }

    /// 这一种支不支持层级。只有分类支持：标签是横着贴的，分层没有意义。
    pub fn is_hierarchical(self) -> bool {
        matches!(self, Self::Category)
    }
}

/// 一个标签。
///
/// 它仍然是扁平的，但有一个稳定 id：仓库与标签的关联落在 `repo_tags` 上，
/// 靠 id 关联，所以改标签名不用回头改每一处引用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    pub id: i64,
    pub name: String,
}

/// 加名字的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddName {
    Added,
    /// 已经有同名（忽略大小写）的一项了，什么都没改。
    AlreadyPresent,
    /// 空名字或超过 [`MAX_NAME_CHARS`]。
    Invalid,
    /// 写库失败，这次改动整个丢了。界面上另有提示说明原因（一般是磁盘或权限）。
    Failed,
}

/// 改名字的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameName {
    Renamed,
    /// 新旧名字算同一个，什么都没动，原有写法也留着。
    Unchanged,
    /// 目标名字已经被别人占了。
    NameTaken,
    /// 要改的那个名字不在词表里。
    NotFound,
    /// 新名字不合法。
    Invalid,
    /// 分类改到了它自己的下级里，树会断成一个环。
    Circular,
    /// 写库失败，这次改动整个丢了。
    Failed,
}

/// 收干净用户输入的名字。返回 `None` 表示这个名字不能用。
///
/// 首尾空白去掉、内部连续空白压成一个空格——`"AI  工具"` 与 `" AI 工具 "`
/// 是同一个人想说的同一件事。
pub fn clean_name(raw: &str) -> Option<String> {
    let cleaned = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() || cleaned.chars().count() > MAX_NAME_CHARS {
        return None;
    }
    Some(cleaned)
}

/// 名字的身份比较：忽略大小写。`AI` 和 `ai` 是同一个分类。
pub fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// 在扁平词表里找名字，返回它的下标。
pub fn index_of(names: &[String], name: &str) -> Option<usize> {
    names.iter().position(|existing| same_name(existing, name))
}

pub fn contains(names: &[String], name: &str) -> bool {
    index_of(names, name).is_some()
}

/// 追加一个名字。已经有同名的就什么都不做并返回 `false`，原有写法保留。
pub fn push_new(names: &mut Vec<String>, raw: &str) -> bool {
    let Some(name) = clean_name(raw) else {
        return false;
    };
    if contains(names, &name) {
        return false;
    }
    names.push(name);
    true
}

// ---- 路径语法 ----

/// 把一条路径拆成各级名字。空段直接丢掉，所以 `""` 会得到空列表。
pub fn segments(path: &str) -> Vec<String> {
    path.split(PATH_SEPARATOR)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// 由各级名字拼回一条路径。
pub fn join_path(parts: &[String]) -> String {
    parts.join(&PATH_SEPARATOR.to_string())
}

/// 收干净用户输入的一条分类路径：逐级清洗，任何一级不合法整条就不合法。
///
/// 两端的空白和多余的分隔符会去掉（`"开发/"` 就是 `"开发"`），但中间的空段
/// （`"开发//前端"`）算误输入——多半是多敲了一个斜杠，不如让他看见。
pub fn clean_path(raw: &str) -> Option<String> {
    let normalized = raw.replace('／', &PATH_SEPARATOR.to_string());
    let trimmed = normalized.trim_matches(|c: char| c.is_whitespace() || c == PATH_SEPARATOR);
    if trimmed.is_empty() {
        return None;
    }

    let parts: Vec<String> = trimmed
        .split(PATH_SEPARATOR)
        .map(clean_name)
        .collect::<Option<Vec<_>>>()?;
    if parts.len() > MAX_DEPTH {
        return None;
    }
    Some(join_path(&parts))
}

/// 去重用的键：名字的小写形式。库里 `name_key` 列存的就是它。
pub fn name_key(name: &str) -> String {
    name.to_lowercase()
}

/// 分类的色相（0..360）。同一个名字永远得到同一个色相，相邻的名字也不会挤在
/// 一起——不为好看，只为「一眼认出是不是同一类」。
pub fn hue_seed(name: &str) -> u16 {
    // FNV-1a：短字符串上分布够匀，实现短到不用引依赖。
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.to_lowercase().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    // 乘一个和 360 互质的步进量，让名字列表里相邻的两项落到不同色相上。
    ((hash.wrapping_mul(137)) % 360) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn clean_name_trims_and_squeezes_whitespace() {
        assert_eq!(clean_name("  工具  ").as_deref(), Some("工具"));
        assert_eq!(clean_name("AI\t工具").as_deref(), Some("AI 工具"));
        assert_eq!(clean_name("周榜 ·  全部语言").as_deref(), Some("周榜 · 全部语言"));
    }

    #[test]
    fn clean_name_rejects_empty_and_overlong_input() {
        assert_eq!(clean_name(""), None);
        assert_eq!(clean_name("   "), None);
        assert_eq!(clean_name("　"), None, "全角空格也是空白");
        assert_eq!(
            clean_name(&"x".repeat(MAX_NAME_CHARS)),
            Some("x".repeat(MAX_NAME_CHARS))
        );
        assert_eq!(clean_name(&"x".repeat(MAX_NAME_CHARS + 1)), None);
    }

    #[test]
    fn name_identity_ignores_case() {
        assert!(same_name("AI", "ai"));
        assert!(same_name("Rust", "rUsT"));
        assert!(!same_name("AI", "AI 工具"));
    }

    #[test]
    fn push_new_keeps_the_first_spelling() {
        let mut list = names(&["AI"]);
        assert!(!push_new(&mut list, "ai"), "同名不该重复添加");
        assert_eq!(list, names(&["AI"]), "第一次的写法要留着");

        assert!(push_new(&mut list, " 命令行 "));
        assert_eq!(list, names(&["AI", "命令行"]));

        assert!(!push_new(&mut list, "   "), "不能用的名字不该进词表");
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn index_of_uses_the_same_identity_as_push_new() {
        let list = names(&["AI", "命令行"]);
        assert_eq!(index_of(&list, "ai"), Some(0));
        assert_eq!(index_of(&list, "命令行"), Some(1));
        assert_eq!(index_of(&list, "没有"), None);
    }

    #[test]
    fn clean_path_cleans_each_level() {
        assert_eq!(clean_path("开发/前端").as_deref(), Some("开发/前端"));
        assert_eq!(clean_path("  开发 / 前端  ").as_deref(), Some("开发/前端"));
        assert_eq!(clean_path("开发/  前端  框架").as_deref(), Some("开发/前端 框架"));
        assert_eq!(clean_path("／开发／前端").as_deref(), Some("开发/前端"), "全角斜杠也认");
        assert_eq!(clean_path("开发/").as_deref(), Some("开发"), "结尾多敲的斜杠不算错");
        assert_eq!(clean_path("开发").as_deref(), Some("开发"));
    }

    #[test]
    fn clean_path_rejects_what_cannot_be_a_category() {
        assert_eq!(clean_path(""), None);
        assert_eq!(clean_path("  /  "), None);
        assert_eq!(clean_path("开发//前端"), None, "中间的空段多半是多敲了斜杠");
        assert_eq!(
            clean_path(&format!("开发/{}", "x".repeat(MAX_NAME_CHARS + 1))),
            None,
            "哪一级太长整条都不合法"
        );
        let deep = (0..=MAX_DEPTH)
            .map(|i| format!("L{i}"))
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(clean_path(&deep), None, "超过层级上限");
        assert!(clean_path(&format!("开发/{}", "x".repeat(MAX_NAME_CHARS))).is_some());
    }

    #[test]
    fn a_path_splits_into_its_levels() {
        assert_eq!(segments("开发/前端/框架"), names(&["开发", "前端", "框架"]));
        assert_eq!(segments(""), Vec::<String>::new());
        assert_eq!(segments("开发//前端"), names(&["开发", "前端"]), "空段直接跳过");
        assert_eq!(join_path(&names(&["开发", "前端"])), "开发/前端");
    }

    #[test]
    fn hue_seed_is_stable_in_range_and_spread_out() {
        let samples = ["AI", "工具", "命令行", "Web", "数据库", "游戏", "安全", "文档"];
        let hues: Vec<u16> = samples.iter().map(|name| hue_seed(name)).collect();

        for hue in &hues {
            assert!(*hue < 360, "色相必须落在 0..360，得到 {hue}");
        }
        // 同一个名字每次都得到同一个色相，否则颜色会随重绘乱跳。
        assert_eq!(hue_seed("AI"), hue_seed("AI"));
        assert_eq!(hue_seed("AI"), hue_seed("ai"), "大小写不该换颜色");

        let unique: HashSet<u16> = hues.iter().copied().collect();
        assert!(
            unique.len() >= samples.len() - 1,
            "8 个常见分类最多允许撞色一次：{hues:?}"
        );
    }
}
